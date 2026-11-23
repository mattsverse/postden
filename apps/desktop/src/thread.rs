//! A thread laid out as a graph, as `git log --graph` lays out commits: one row per message,
//! oldest first, each in a lane. A reply continues the lane of the message it replies to;
//! a second reply to the same message forks into a lane of its own, which stays taken until its
//! last message, so lines never cross a lane in use.

use std::collections::HashSet;

use postden_store::StoredMessage;

/// What a row draws in the lanes of its graph cell, besides its own message's dot. Top and
/// bottom are the halves of the row above and below its middle, where the dot sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Segment {
    /// A line through the whole row, in a lane passing by.
    Through { lane: usize, dashed: bool },
    /// A line from the top of the row down to its dot.
    Top { lane: usize, dashed: bool },
    /// A line from its dot down to the bottom of the row.
    Bottom { lane: usize, dashed: bool },
    /// A curve from its dot, in lane `from`, down to the bottom of the row in lane `to`: where
    /// a second reply forks off.
    Fork {
        from: usize,
        to: usize,
        dashed: bool,
    },
}

/// One message of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) message: StoredMessage,
    /// The lane its dot is in.
    pub(crate) lane: usize,
    /// The row of the message it replies to, if that's in the thread.
    pub(crate) parent: Option<usize>,
    /// It replies to a message that isn't cached, as one older than what was synced.
    pub(crate) parent_missing: bool,
    /// What the row draws besides its dot, in its lanes and those passing by.
    pub(crate) segments: Vec<Segment>,
    /// How its text begins, when that's downloaded, as [`snippet`] gives it.
    pub(crate) snippet: Option<String>,
}

/// A thread laid out as a graph, oldest message first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Thread {
    pub(crate) rows: Vec<Row>,
    /// How many lanes it takes, side by side.
    pub(crate) lanes: usize,
}

impl Thread {
    /// Lays out `messages`, oldest first, as [`postden_store::Store::thread`] gives them;
    /// though a reply dated before the message it replies to, by a clock that's off, comes
    /// after it.
    pub(crate) fn new(messages: Vec<StoredMessage>) -> Self {
        let messages = replies_after(messages);
        let parents = parents(&messages);
        let (branches, branch_of) = branches(&parents);
        let (lane_of_branch, lanes) = lanes(&branches);
        let lane = |row: usize| {
            branch_of
                .get(row)
                .and_then(|b| lane_of_branch.get(*b))
                .copied()
                .unwrap_or_default()
        };
        let mut segments = segments(&parents, &branches, &lane_of_branch, &lane);
        let rows = messages
            .into_iter()
            .zip(parents)
            .enumerate()
            .map(|(i, (message, parent))| Row {
                message,
                lane: lane(i),
                parent: parent.row,
                parent_missing: parent.missing,
                segments: segments.get_mut(i).map(std::mem::take).unwrap_or_default(),
                snippet: None,
            })
            .collect();
        Self { rows, lanes }
    }

    /// The row of the message `key` names, if it's in the thread.
    pub(crate) fn position(&self, key: &crate::search::MessageKey) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| crate::state::message_key(&r.message) == *key)
    }
}

/// `messages`, oldest first, each moved after the message it replies to where that's dated
/// later, or the same: as a clock that's off, or a reply sent within the second, would have it.
fn replies_after(messages: Vec<StoredMessage>) -> Vec<StoredMessage> {
    let ids: std::collections::HashMap<&str, usize> = messages
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(i, m)| Some((m.envelope.message_id.as_deref()?, i)))
        .collect();
    let parent: Vec<Option<usize>> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let e = &m.envelope;
            let found = |id: &String| ids.get(id.as_str()).copied().filter(|&p| p != i);
            e.in_reply_to
                .as_ref()
                .and_then(found)
                .or_else(|| e.references.iter().rev().find_map(found))
        })
        .collect();
    let mut placed = vec![false; messages.len()];
    let mut order = Vec::with_capacity(messages.len());
    while order.len() < messages.len() {
        let ready = |i: usize| {
            !placed.get(i).copied().unwrap_or(true)
                && parent
                    .get(i)
                    .copied()
                    .flatten()
                    .is_none_or(|p| placed.get(p).copied().unwrap_or(true))
        };
        // The oldest whose parent is placed; or, where replies point at each other in a
        // circle, the oldest left.
        let next = (0..messages.len())
            .find(|&i| ready(i))
            .or_else(|| (0..messages.len()).find(|&i| !placed.get(i).copied().unwrap_or(true)));
        let Some(next) = next else { break };
        if let Some(done) = placed.get_mut(next) {
            *done = true;
        }
        order.push(next);
    }
    let mut messages: Vec<Option<StoredMessage>> = messages.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|i| messages.get_mut(i).and_then(Option::take))
        .collect()
}

/// The message a message replies to, as far as its thread is cached.
#[derive(Debug, Clone, Copy, Default)]
struct Parent {
    /// Its row, if it's cached.
    row: Option<usize>,
    /// Found by its References rather than its In-Reply-To: messages between may be missing.
    inferred: bool,
    /// It replies to a message, but none of its thread before it is cached.
    missing: bool,
}

/// Each message's parent: the message it says it replies to, if that's cached and older; else
/// the latest of its References that is.
fn parents(messages: &[StoredMessage]) -> Vec<Parent> {
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let earlier = messages.get(..i).unwrap_or_default();
            let row_of = |id: &String| {
                earlier
                    .iter()
                    .position(|e| e.envelope.message_id.as_ref() == Some(id))
            };
            let e = &m.envelope;
            if let Some(row) = e.in_reply_to.as_ref().and_then(row_of) {
                return Parent {
                    row: Some(row),
                    ..Parent::default()
                };
            }
            let inferred = e.references.iter().rev().find_map(row_of);
            // Without In-Reply-To, the last of its References is the message it replies to:
            // only an earlier one stands in for a message missing between.
            let direct = e.in_reply_to.is_none()
                && inferred.is_some()
                && e.references.last().and_then(row_of) == inferred;
            Parent {
                row: inferred,
                inferred: inferred.is_some() && !direct,
                missing: inferred.is_none()
                    && (e.in_reply_to.is_some() || !e.references.is_empty()),
            }
        })
        .collect()
}

/// A run of messages drawn in one lane: each one's first reply continues it; later replies
/// start branches of their own.
#[derive(Debug, Default)]
struct Branch {
    /// The row it forks from, unless it starts the thread, or a part of it whose start isn't
    /// cached.
    fork: Option<usize>,
    /// Its messages' rows, in order.
    rows: Vec<usize>,
}

impl Branch {
    /// The rows it takes a lane for: from where it forks, or its first, to its last.
    fn span(&self) -> (usize, usize) {
        let first = self.rows.first().copied().unwrap_or_default();
        let last = self.rows.last().copied().unwrap_or(first);
        (self.fork.unwrap_or(first), last)
    }
}

/// The thread's branches, and the branch each row is in.
fn branches(parents: &[Parent]) -> (Vec<Branch>, Vec<usize>) {
    let mut branches: Vec<Branch> = Vec::new();
    let mut branch_of: Vec<usize> = Vec::with_capacity(parents.len());
    let mut continued: HashSet<usize> = HashSet::new();
    for (row, parent) in parents.iter().enumerate() {
        let continuing = parent
            .row
            .filter(|p| continued.insert(*p))
            .and_then(|p| branch_of.get(p).copied());
        if let Some(b) = continuing
            && let Some(branch) = branches.get_mut(b)
        {
            branch.rows.push(row);
            branch_of.push(b);
        } else {
            branch_of.push(branches.len());
            branches.push(Branch {
                fork: parent.row,
                rows: vec![row],
            });
        }
    }
    (branches, branch_of)
}

/// Each branch's lane: the leftmost free from the row it forks from to its last. And how many
/// lanes that takes.
fn lanes(branches: &[Branch]) -> (Vec<usize>, usize) {
    let mut taken: Vec<Vec<(usize, usize)>> = Vec::new();
    let lanes = branches
        .iter()
        .map(|branch| {
            let (start, end) = branch.span();
            let free =
                |spans: &Vec<(usize, usize)>| spans.iter().all(|&(s, e)| end < s || e < start);
            let lane = taken.iter().position(free).unwrap_or(taken.len());
            if let Some(spans) = taken.get_mut(lane) {
                spans.push((start, end));
            } else {
                taken.push(vec![(start, end)]);
            }
            lane
        })
        .collect();
    (lanes, taken.len())
}

/// What each row draws besides its dot.
fn segments(
    parents: &[Parent],
    branches: &[Branch],
    lane_of_branch: &[usize],
    lane: &impl Fn(usize) -> usize,
) -> Vec<Vec<Segment>> {
    let mut segments: Vec<Vec<Segment>> = vec![Vec::new(); parents.len()];
    let mut put = |row: usize, segment: Segment| {
        if let Some(row) = segments.get_mut(row) {
            row.push(segment);
        }
    };
    let inferred = |row: usize| parents.get(row).is_some_and(|p| p.inferred);
    for (branch, &l) in branches.iter().zip(lane_of_branch) {
        let Some(&first) = branch.rows.first() else {
            continue;
        };
        if let Some(p) = branch.fork {
            let dashed = inferred(first);
            put(
                p,
                Segment::Fork {
                    from: lane(p),
                    to: l,
                    dashed,
                },
            );
            for row in p + 1..first {
                put(row, Segment::Through { lane: l, dashed });
            }
            put(first, Segment::Top { lane: l, dashed });
        } else if parents.get(first).is_some_and(|p| p.missing) {
            put(
                first,
                Segment::Top {
                    lane: l,
                    dashed: true,
                },
            );
        }
        for (&from, &to) in branch.rows.iter().zip(branch.rows.iter().skip(1)) {
            let dashed = inferred(to);
            put(from, Segment::Bottom { lane: l, dashed });
            for row in from + 1..to {
                put(row, Segment::Through { lane: l, dashed });
            }
            put(to, Segment::Top { lane: l, dashed });
        }
    }
    segments
}

/// The most characters [`snippet`] keeps.
const SNIPPET_CHARS: usize = 90;

/// Whether `line` says who wrote what a reply quotes under it: "On Monday, Ada wrote:".
fn is_attribution(line: &str) -> bool {
    line.starts_with("On ") && line.ends_with(':')
}

/// How a message's `text` begins, on one line: its first line that isn't quoted from the message
/// it replies to, nor the line saying who wrote that; or `None` when it says nothing else.
pub(crate) fn snippet(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('>') && !is_attribution(line))?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let line = words.join(" ");
    Some(if line.chars().count() > SNIPPET_CHARS {
        let cut: String = line.chars().take(SNIPPET_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        line
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use postden_core::message::{Address, Envelope};

    /// Message `id`, replying to `parent`, with `references`.
    fn message(id: u32, parent: Option<u32>, references: &[u32]) -> StoredMessage {
        let name = |n: u32| format!("{n}@example.com");
        StoredMessage {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            envelope: Envelope {
                uid: id,
                message_id: Some(name(id)),
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: Some(i64::from(id)),
                seen: true,
                flagged: false,
                in_reply_to: parent.map(name),
                references: references.iter().copied().map(name).collect(),
            },
        }
    }

    fn lanes(thread: &Thread) -> Vec<usize> {
        thread.rows.iter().map(|r| r.lane).collect()
    }

    use Segment::{Bottom, Fork, Through, Top};

    #[test]
    fn a_conversation_back_and_forth_is_one_lane() {
        let thread = Thread::new(vec![
            message(1, None, &[]),
            message(2, Some(1), &[1]),
            message(3, Some(2), &[1, 2]),
        ]);
        assert_eq!(lanes(&thread), vec![0, 0, 0]);
        assert_eq!(thread.lanes, 1);
        let solid = |lane| {
            (
                Top {
                    lane,
                    dashed: false,
                },
                Bottom {
                    lane,
                    dashed: false,
                },
            )
        };
        let (top, bottom) = solid(0);
        assert_eq!(thread.rows[0].segments, vec![bottom]);
        assert_eq!(thread.rows[1].segments, vec![top, bottom]);
        assert_eq!(thread.rows[2].segments, vec![top]);
    }

    #[test]
    fn a_second_reply_forks_into_a_lane_of_its_own_until_its_last_message() {
        // 1 asks; 2 and 3 both answer 1; 4 answers 2; 5 answers 3; 6 answers 4.
        let thread = Thread::new(vec![
            message(1, None, &[]),
            message(2, Some(1), &[1]),
            message(3, Some(1), &[1]),
            message(4, Some(2), &[1, 2]),
            message(5, Some(3), &[1, 3]),
            message(6, Some(4), &[1, 2, 4]),
        ]);
        assert_eq!(lanes(&thread), vec![0, 0, 1, 0, 1, 0]);
        assert_eq!(thread.lanes, 2);
        let fork = Fork {
            from: 0,
            to: 1,
            dashed: false,
        };
        assert!(thread.rows[0].segments.contains(&fork));
        // The fork's lane passes 2 by, on its way to 3.
        assert!(thread.rows[1].segments.contains(&Through {
            lane: 1,
            dashed: false
        }));
        assert_eq!(thread.rows[2].parent, Some(0));
    }

    #[test]
    fn a_lane_freed_is_taken_again() {
        // 2 and 3 answer 1, and 3's side ends there; 5 and 6 answer 4, after it.
        let thread = Thread::new(vec![
            message(1, None, &[]),
            message(2, Some(1), &[1]),
            message(3, Some(1), &[1]),
            message(4, Some(2), &[1, 2]),
            message(5, Some(4), &[1, 2, 4]),
            message(6, Some(4), &[1, 2, 4]),
        ]);
        assert_eq!(lanes(&thread), vec![0, 0, 1, 0, 0, 1]);
        assert_eq!(thread.lanes, 2);
    }

    #[test]
    fn a_reply_to_a_message_not_cached_hangs_from_a_dashed_line() {
        // 2 is missing: 3 replies to it, its References reach back to 1.
        let thread = Thread::new(vec![message(1, None, &[]), message(3, Some(2), &[1, 2])]);
        assert_eq!(lanes(&thread), vec![0, 0]);
        assert_eq!(thread.rows[1].parent, Some(0));
        assert_eq!(
            thread.rows[1].segments,
            vec![Top {
                lane: 0,
                dashed: true
            }]
        );
        // And with nothing of its thread cached, it starts one, from a dashed stub.
        let alone = Thread::new(vec![message(3, Some(2), &[2])]);
        assert!(alone.rows[0].parent_missing);
        assert_eq!(
            alone.rows[0].segments,
            vec![Top {
                lane: 0,
                dashed: true
            }]
        );
    }

    #[test]
    fn a_snippet_is_what_the_reply_says_not_what_it_quotes() {
        let text = "\n  Great,   see you then!\n\nOn Mon, Ada wrote:\n> Lunch?";
        assert_eq!(snippet(text).as_deref(), Some("Great, see you then!"));
        assert_eq!(snippet("On Mon, Ada wrote:\n> Lunch?"), None);
        let long = "word ".repeat(40);
        let cut = snippet(&long).unwrap();
        assert!(cut.ends_with('…') && cut.chars().count() <= SNIPPET_CHARS);
    }

    #[test]
    fn a_reply_dated_before_what_it_answers_still_hangs_from_it() {
        // 2 replies to 1 but is dated before it, as by a clock that's off.
        let mut early = message(2, Some(1), &[1]);
        early.envelope.date = Some(0);
        let thread = Thread::new(vec![early, message(1, None, &[])]);
        let order: Vec<u32> = thread.rows.iter().map(|r| r.message.envelope.uid).collect();
        assert_eq!(order, vec![1, 2]);
        assert_eq!(thread.rows[1].parent, Some(0));
        // Replies pointing at each other in a circle are each shown, without looping.
        let circle = Thread::new(vec![message(1, Some(2), &[2]), message(2, Some(1), &[1])]);
        assert_eq!(circle.rows.len(), 2);
    }

    #[test]
    fn a_reply_naming_its_parent_only_in_its_references_hangs_from_a_solid_line() {
        let thread = Thread::new(vec![message(1, None, &[]), message(2, None, &[1])]);
        assert_eq!(thread.rows[1].parent, Some(0));
        assert_eq!(
            thread.rows[1].segments,
            vec![Top {
                lane: 0,
                dashed: false
            }]
        );
    }
}
