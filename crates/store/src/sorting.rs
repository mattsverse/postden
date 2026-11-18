//! What sorting mail learned, kept for each account: the classes of each of its models, their
//! weights, and what each message was taught as.
//!
//! One thread learns, and writes what it learned with [`Store::save_sorting`]. Anything else that
//! changes what's kept, as [`Store::forget_sorting`] and [`Store::replace_sorting`] do, moves
//! the kept learning to a new generation, so what was learned from an older one isn't saved over
//! it.

use rusqlite::{Transaction, TransactionBehavior, params};

use crate::{Error, Result, Store};

/// The setting the generation of what's kept is in.
const GENERATION: &str = "sorting_generation";
/// The version of the features what's kept was learned by.
const FEATURES: &str = "sorting_features";

/// One class of an account's model, and its weights: all of them as [`Store::sorting`] reads
/// them, or those changed as [`Store::save_sorting`] writes them.
#[derive(Debug, Clone, PartialEq)]
pub struct SortingClass {
    pub account: String,
    /// Which of the account's models it's of.
    pub model: String,
    pub name: String,
    /// How many messages were taught as it.
    pub messages: u32,
    /// How many messages it was the answer for, asked before being taught them.
    pub guessed: u32,
    /// How many of those it was right for.
    pub correct: u32,
    pub weights: Vec<SortingWeight>,
}

/// What a class learned of one feature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SortingWeight {
    pub feature: u64,
    pub z: f32,
    pub n: f32,
}

/// What a message was taught as, by its Message-ID, for one of an account's models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortingTaught {
    pub account: String,
    pub model: String,
    pub message_id: String,
    pub class: String,
}

/// Everything sorting mail learned, as kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sorting {
    pub classes: Vec<SortingClass>,
    pub taught: Vec<SortingTaught>,
}

/// What learning changed, for [`Store::save_sorting`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SortingChanges {
    /// Classes forgotten, by account, model and name, with what was taught as them. They go
    /// first, so a class forgotten and learned again starts afresh.
    pub removed: Vec<(String, String, String)>,
    /// Classes learned, with their weights that changed.
    pub classes: Vec<SortingClass>,
    pub taught: Vec<SortingTaught>,
}

/// A feature as SQLite keeps it: the same bits, as SQLite's integers are signed.
const fn feature_to_sql(feature: u64) -> i64 {
    i64::from_ne_bytes(feature.to_ne_bytes())
}

const fn feature_from_sql(feature: i64) -> u64 {
    u64::from_ne_bytes(feature.to_ne_bytes())
}

fn count(value: i64) -> Result<u32> {
    u32::try_from(value).map_err(|e| Error::Corrupt(e.to_string()))
}

#[expect(
    clippy::multiple_inherent_impl,
    reason = "the store's sorting is a part of its own, in a file of its own"
)]
impl Store {
    /// The generation of what sorting mail learned that's kept: another once it's forgotten or
    /// replaced.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or the generation is corrupt.
    pub fn sorting_generation(&self) -> Result<u64> {
        generation(&self.conn)
    }

    /// Everything sorting mail learned that's kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn sorting(&self) -> Result<Sorting> {
        Ok(self.sorting_learned()?.1)
    }

    /// Everything sorting mail learned that's kept, and the version of the features it was
    /// learned by, if it was ever said.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn sorting_learned(&self) -> Result<(Option<u32>, Sorting)> {
        // All at once, as the sorter may save what it learned meanwhile.
        let tx = self.conn.unchecked_transaction()?;
        let features = features(&tx)?;
        let mut classes = Vec::new();
        let mut ids = std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, account, model, name, messages, guessed, correct
                 FROM sorting_classes ORDER BY id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                ids.insert(row.get::<_, i64>(0)?, classes.len());
                classes.push(SortingClass {
                    account: row.get(1)?,
                    model: row.get(2)?,
                    name: row.get(3)?,
                    messages: count(row.get(4)?)?,
                    guessed: count(row.get(5)?)?,
                    correct: count(row.get(6)?)?,
                    weights: Vec::new(),
                });
            }
        }
        let mut stmt = tx.prepare("SELECT class, feature, z, n FROM sorting_weights")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let class: i64 = row.get(0)?;
            let Some(class) = ids.get(&class).and_then(|&i| classes.get_mut(i)) else {
                return Err(Error::Corrupt(format!("a weight of no class {class}")));
            };
            class.weights.push(SortingWeight {
                feature: feature_from_sql(row.get(1)?),
                z: row.get(2)?,
                n: row.get(3)?,
            });
        }
        let mut stmt =
            tx.prepare("SELECT account, model, message_id, class FROM sorting_taught")?;
        let taught = stmt
            .query_map([], |row| {
                Ok(SortingTaught {
                    account: row.get(0)?,
                    model: row.get(1)?,
                    message_id: row.get(2)?,
                    class: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok((features, Sorting { classes, taught }))
    }

    /// Saves what was learned from what was kept at `generation`, all or nothing. Returns
    /// `false`, having saved nothing, when what's kept is of another generation since: it was
    /// forgotten or replaced, and what was learned from it is stale.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails, as when an account isn't saved.
    pub fn save_sorting(&mut self, generation: u64, changes: &SortingChanges) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if self::generation(&tx)? != generation {
            return Ok(false);
        }
        for (account, model, name) in &changes.removed {
            tx.execute(
                "DELETE FROM sorting_classes WHERE account = ?1 AND model = ?2 AND name = ?3",
                [account, model, name],
            )?;
            tx.execute(
                "DELETE FROM sorting_taught WHERE account = ?1 AND model = ?2 AND class = ?3",
                [account, model, name],
            )?;
        }
        write_sorting(&tx, &changes.classes, &changes.taught)?;
        tx.commit()?;
        Ok(true)
    }

    /// Moves what sorting mail learned to a new generation, forgetting nothing: what was learned
    /// from it meanwhile, as by a pass under way when sorting was turned off, isn't saved.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn renew_sorting(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        next_generation(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets everything sorting mail learned.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails.
    pub fn forget_sorting(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sorting_classes", [])?;
        tx.execute("DELETE FROM sorting_taught", [])?;
        next_generation(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets everything sorting mail learned unless it was learned by the version `features`
    /// of the features, which what's learned from now on is: learned by others, it says nothing
    /// of the mail as it's seen now.
    ///
    /// # Errors
    ///
    /// Returns an error when the read or a write fails.
    pub fn keep_sorting_learned_by(&mut self, features: u32) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if self::features(&tx)? != Some(features) {
            tx.execute("DELETE FROM sorting_classes", [])?;
            tx.execute("DELETE FROM sorting_taught", [])?;
            next_generation(&tx)?;
            set(&tx, FEATURES, &features.to_string())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replaces what sorting mail learned of each of `accounts` with what `sorting` has of it,
    /// learned by the version `features` of the features, all or nothing. What `sorting` has of
    /// other accounts is left out, and so is what's kept of other accounts.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails, as when an account isn't saved.
    pub fn replace_sorting(
        &mut self,
        accounts: &[&str],
        sorting: &Sorting,
        features: u32,
    ) -> Result<()> {
        // Immediate: what's read is written on, and the sorter may write meanwhile.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // What other features learned of the other accounts says nothing anymore either.
        if self::features(&tx)?.is_some_and(|kept| kept != features) {
            tx.execute("DELETE FROM sorting_classes", [])?;
            tx.execute("DELETE FROM sorting_taught", [])?;
        }
        for account in accounts {
            delete_sorting_of(&tx, account)?;
        }
        let classes: Vec<SortingClass> = sorting
            .classes
            .iter()
            .filter(|c| accounts.contains(&c.account.as_str()))
            .cloned()
            .collect();
        let taught: Vec<SortingTaught> = sorting
            .taught
            .iter()
            .filter(|t| accounts.contains(&t.account.as_str()))
            .cloned()
            .collect();
        write_sorting(&tx, &classes, &taught)?;
        next_generation(&tx)?;
        set(&tx, FEATURES, &features.to_string())?;
        tx.commit()?;
        Ok(())
    }
}

/// The version of the features what's kept was learned by, if it was ever said.
fn features(conn: &rusqlite::Connection) -> Result<Option<u32>> {
    let value: Option<String> = rusqlite::OptionalExtension::optional(conn.query_row(
        "SELECT value FROM settings WHERE name = ?1",
        [FEATURES],
        |row| row.get(0),
    ))?;
    value
        .map(|v| {
            v.parse()
                .map_err(|e| Error::Corrupt(format!("sorting features {v:?}: {e}")))
        })
        .transpose()
}

fn generation(conn: &rusqlite::Connection) -> Result<u64> {
    let value: Option<String> = rusqlite::OptionalExtension::optional(conn.query_row(
        "SELECT value FROM settings WHERE name = ?1",
        [GENERATION],
        |row| row.get(0),
    ))?;
    value.map_or(Ok(0), |v| {
        v.parse()
            .map_err(|e| Error::Corrupt(format!("sorting generation {v:?}: {e}")))
    })
}

/// Moves what's kept to a new generation.
fn next_generation(tx: &Transaction<'_>) -> Result<()> {
    let next = generation(tx)?.wrapping_add(1).to_string();
    set(tx, GENERATION, &next)
}

fn set(tx: &Transaction<'_>, name: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO settings (name, value) VALUES (?1, ?2)
         ON CONFLICT (name) DO UPDATE SET value = excluded.value",
        [name, value],
    )?;
    Ok(())
}

/// Forgets what sorting learned of `account`, in a new generation.
pub(crate) fn forget_sorting_of(tx: &Transaction<'_>, account: &str) -> Result<()> {
    delete_sorting_of(tx, account)?;
    next_generation(tx)
}

fn delete_sorting_of(tx: &Transaction<'_>, account: &str) -> Result<()> {
    tx.execute("DELETE FROM sorting_classes WHERE account = ?1", [account])?;
    tx.execute("DELETE FROM sorting_taught WHERE account = ?1", [account])?;
    Ok(())
}

fn write_sorting(
    tx: &Transaction<'_>,
    classes: &[SortingClass],
    taught: &[SortingTaught],
) -> Result<()> {
    let mut class_stmt = tx.prepare_cached(
        "INSERT INTO sorting_classes (account, model, name, messages, guessed, correct)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (account, model, name) DO UPDATE SET
            messages = excluded.messages, guessed = excluded.guessed, correct = excluded.correct
         RETURNING id",
    )?;
    let mut weight_stmt = tx.prepare_cached(
        "INSERT INTO sorting_weights (class, feature, z, n) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (class, feature) DO UPDATE SET z = excluded.z, n = excluded.n",
    )?;
    for class in classes {
        let id: i64 = class_stmt.query_row(
            params![
                class.account,
                class.model,
                class.name,
                class.messages,
                class.guessed,
                class.correct
            ],
            |row| row.get(0),
        )?;
        for w in &class.weights {
            weight_stmt.execute(params![id, feature_to_sql(w.feature), w.z, w.n])?;
        }
    }
    let mut taught_stmt = tx.prepare_cached(
        "INSERT INTO sorting_taught (account, model, message_id, class) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (account, model, message_id) DO UPDATE SET class = excluded.class",
    )?;
    for t in taught {
        taught_stmt.execute([&t.account, &t.model, &t.message_id, &t.class])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::account;

    fn class(account: &str, name: &str, weights: &[(u64, f32)]) -> SortingClass {
        SortingClass {
            account: account.into(),
            model: "folders".into(),
            name: name.into(),
            messages: 2,
            guessed: 1,
            correct: 1,
            weights: weights
                .iter()
                .map(|&(feature, z)| SortingWeight { feature, z, n: 1.0 })
                .collect(),
        }
    }

    fn taught(account: &str, id: &str, class: &str) -> SortingTaught {
        SortingTaught {
            account: account.into(),
            model: "folders".into(),
            message_id: id.into(),
            class: class.into(),
        }
    }

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&account("a@example.com")).unwrap();
        store.save_account(&account("b@example.com")).unwrap();
        store
    }

    #[test]
    fn what_was_learned_is_saved_and_changed_in_place() {
        let mut store = store();
        let generation = store.sorting_generation().unwrap();
        let changes = SortingChanges {
            removed: Vec::new(),
            classes: vec![class(
                "a@example.com",
                "Receipts",
                &[(1, 0.5), (u64::MAX, -1.0)],
            )],
            taught: vec![taught("a@example.com", "1@x", "Receipts")],
        };
        assert!(store.save_sorting(generation, &changes).unwrap());
        let relearned = SortingChanges {
            removed: Vec::new(),
            classes: vec![SortingClass {
                messages: 3,
                ..class("a@example.com", "Receipts", &[(1, 2.0)])
            }],
            taught: vec![taught("a@example.com", "1@x", "INBOX")],
        };
        assert!(store.save_sorting(generation, &relearned).unwrap());
        let kept = store.sorting().unwrap();
        let mut receipts = class("a@example.com", "Receipts", &[(1, 2.0), (u64::MAX, -1.0)]);
        receipts.messages = 3;
        let mut weights = kept.classes[0].weights.clone();
        weights.sort_by_key(|w| w.feature);
        assert_eq!(
            SortingClass {
                weights,
                ..kept.classes[0].clone()
            },
            receipts
        );
        assert_eq!(kept.taught, vec![taught("a@example.com", "1@x", "INBOX")]);
    }

    #[test]
    fn a_class_removed_takes_its_weights_and_what_was_taught_as_it() {
        let mut store = store();
        let changes = SortingChanges {
            removed: Vec::new(),
            classes: vec![
                class("a@example.com", "Receipts", &[(1, 0.5)]),
                class("a@example.com", "INBOX", &[(2, 0.5)]),
            ],
            taught: vec![
                taught("a@example.com", "1@x", "Receipts"),
                taught("a@example.com", "2@x", "INBOX"),
            ],
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        let removed = SortingChanges {
            removed: vec![("a@example.com".into(), "folders".into(), "Receipts".into())],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &removed).unwrap());
        let kept = store.sorting().unwrap();
        assert_eq!(
            kept.classes,
            vec![class("a@example.com", "INBOX", &[(2, 0.5)])]
        );
        assert_eq!(kept.taught, vec![taught("a@example.com", "2@x", "INBOX")]);
    }

    #[test]
    fn what_other_features_learned_is_forgotten_and_what_was_imported_kept() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        store.keep_sorting_learned_by(2).unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default(), "never said");
        let generation = store.sorting_generation().unwrap();
        assert!(store.save_sorting(generation, &changes).unwrap());
        store.keep_sorting_learned_by(2).unwrap();
        assert_ne!(store.sorting().unwrap(), Sorting::default(), "the same");

        let imported = store.sorting().unwrap();
        store.keep_sorting_learned_by(1).unwrap();
        store
            .replace_sorting(&["a@example.com"], &imported, 3)
            .unwrap();
        store.keep_sorting_learned_by(3).unwrap();
        assert_eq!(store.sorting().unwrap(), imported, "learned by them");
    }

    #[test]
    fn importing_what_other_features_learned_forgets_whats_kept_by_the_old() {
        let mut store = store();
        store.save_account(&account("b@example.com")).unwrap();
        store.keep_sorting_learned_by(1).unwrap();
        let both = SortingChanges {
            classes: vec![
                class("a@example.com", "Receipts", &[(1, 0.5)]),
                class("b@example.com", "Receipts", &[(1, 0.5)]),
            ],
            ..SortingChanges::default()
        };
        let generation = store.sorting_generation().unwrap();
        assert!(store.save_sorting(generation, &both).unwrap());
        let imported = Sorting {
            classes: vec![class("a@example.com", "Bills", &[(2, 0.25)])],
            taught: Vec::new(),
        };
        store
            .replace_sorting(&["a@example.com"], &imported, 2)
            .unwrap();
        assert_eq!(
            store.sorting_learned().unwrap(),
            (Some(2), imported),
            "b's was learned by the old features"
        );
    }

    #[test]
    fn an_account_saved_for_another_server_forgets_where_its_mail_went() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        store.save_account(&account("a@example.com")).unwrap();
        assert_ne!(store.sorting().unwrap(), Sorting::default(), "the same");
        let mut moved = account("a@example.com");
        moved.imap.username = "someone-else".into();
        store.save_account(&moved).unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default());
    }

    #[test]
    fn what_was_learned_before_sorting_was_turned_off_isnt_saved() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        store.renew_sorting().unwrap();
        assert!(!store.save_sorting(0, &changes).unwrap(), "learned before");
        assert_ne!(
            store.sorting().unwrap(),
            Sorting::default(),
            "nothing forgotten"
        );
    }

    #[test]
    fn what_was_learned_before_forgetting_isnt_saved() {
        let mut store = store();
        let generation = store.sorting_generation().unwrap();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(generation, &changes).unwrap());
        store.forget_sorting().unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default());
        assert!(
            !store.save_sorting(generation, &changes).unwrap(),
            "learned from what was forgotten"
        );
        assert_eq!(store.sorting().unwrap(), Sorting::default());
        let now = store.sorting_generation().unwrap();
        assert!(store.save_sorting(now, &changes).unwrap());
    }

    #[test]
    fn replacing_takes_only_the_accounts_replaced() {
        let mut store = store();
        let kept = SortingChanges {
            classes: vec![
                class("a@example.com", "Old", &[(1, 0.5)]),
                class("b@example.com", "Theirs", &[(1, 0.5)]),
            ],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &kept).unwrap());
        let imported = Sorting {
            classes: vec![
                class("a@example.com", "New", &[(3, 1.0)]),
                class("c@example.com", "Unknown", &[(3, 1.0)]),
            ],
            taught: vec![taught("a@example.com", "1@x", "New")],
        };
        store
            .replace_sorting(&["a@example.com"], &imported, 1)
            .unwrap();
        let names: Vec<_> = store
            .sorting()
            .unwrap()
            .classes
            .into_iter()
            .map(|c| (c.account, c.name))
            .collect();
        assert_eq!(
            names,
            vec![
                ("b@example.com".into(), "Theirs".into()),
                ("a@example.com".into(), "New".into())
            ]
        );
        assert_eq!(store.sorting_generation().unwrap(), 1);
    }

    #[test]
    fn an_account_removed_takes_what_was_learned_of_it() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            taught: vec![taught("a@example.com", "1@x", "Receipts")],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        store.remove_account("a@example.com").unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default());
    }

    #[test]
    fn an_account_moved_to_another_server_forgets_where_its_mail_went() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        let mut moved = account("a@example.com");
        moved.imap.host = "imap.elsewhere.example".into();
        store.replace_account(&moved, None).unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default());
        assert!(
            !store.save_sorting(0, &changes).unwrap(),
            "what was learned of the old server's isn't saved"
        );
    }

    #[test]
    fn an_account_removed_forgets_where_its_mail_went() {
        let mut store = store();
        let changes = SortingChanges {
            classes: vec![class("a@example.com", "Receipts", &[(1, 0.5)])],
            ..SortingChanges::default()
        };
        assert!(store.save_sorting(0, &changes).unwrap());
        assert!(store.remove_account("a@example.com").unwrap());
        store.save_account(&account("a@example.com")).unwrap();
        assert_eq!(store.sorting().unwrap(), Sorting::default());
        assert!(
            !store.save_sorting(0, &changes).unwrap(),
            "what was learned before it was removed isn't saved"
        );
    }
}
