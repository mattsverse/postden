//! Logistic regression over sets of hashed features, learned one message at a time, with
//! FTRL-Proximal: each feature learns at its own pace, fast while it's new and slower as it's seen
//! more, which suits mail, where a sender may be seen once or a thousand times.
//!
//! It's a softmax over classes that come and go, as mailboxes do. A class only keeps a weight for
//! a feature once a message of its own had it: a feature says nothing of a class that never saw
//! it, so a model stays about as small as the mail it learned from, however many classes it has.
//! Each class's bias is the exception, learned from every message, so a class seen rarely is
//! unlikely until its features say otherwise.

use std::collections::{HashMap, HashSet};

/// How fast weights move. Mail is learned from a few messages at a time, so it's quick.
const ALPHA: f32 = 1.0;
/// Smooths how fast a feature seen only a few times learns.
const BETA: f32 = 1.0;
/// Pulls weights barely earned to zero.
const L1: f32 = 0.01;
/// Keeps weights from growing without end, so one feature can't decide alone.
const L2: f32 = 0.1;

/// The feature every message has: what a class's bias is learned as.
pub(crate) const BIAS: u64 = 0;

/// How far a class must be from what it should have said of a message for its features to learn
/// from it: a class that was never in the running learns too little to be worth the while, but
/// its bias.
const MIN_GRADIENT: f32 = 0.005;

/// What a class learned of one feature, as FTRL-Proximal keeps it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Weight {
    /// The sum of what it was taught, less how far it moved.
    pub z: f32,
    /// The sum of its squared gradients: how much it was taught.
    pub n: f32,
}

impl Weight {
    fn value(self) -> f32 {
        if self.z.abs() <= L1 {
            return 0.0;
        }
        -(self.z - L1.copysign(self.z)) / (BETA + self.n.sqrt()).mul_add(ALPHA.recip(), L2)
    }

    fn learn(&mut self, gradient: f32) {
        let before = self.value();
        let n = gradient.mul_add(gradient, self.n);
        let sigma = (n.sqrt() - self.n.sqrt()) / ALPHA;
        self.z += sigma.mul_add(-before, gradient);
        self.n = n;
    }
}

/// One class a [`Model`] tells apart: a mailbox, or whether mail matters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Class {
    pub name: String,
    /// How many messages it was taught were of it.
    pub messages: u32,
    /// How many messages it was the answer for, asked before being taught them.
    pub guessed: u32,
    /// How many of those were right.
    pub right: u32,
}

/// When a class was the answer this many times, what it was answered is halved, so what it did
/// lately counts the most.
const GUESSES_KEPT: u32 = 100;
/// How many messages a class must have been the answer for, rightly, before it's trusted.
const RIGHT_ENOUGH: u32 = 3;
/// How often a class must be right, of the times it was the answer, to be trusted.
const PRECISION: f64 = 0.8;

impl Class {
    /// A class nothing was taught as yet.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// Whether its answers can be trusted: it was rightly the answer often enough, and wrong
    /// rarely.
    pub(crate) fn is_trusted(&self) -> bool {
        self.right >= RIGHT_ENOUGH && f64::from(self.right) >= PRECISION * f64::from(self.guessed)
    }
}

/// What learning changed in a [`Model`], to save.
///
/// The classes forgotten go first, as one may be learned again since; then each class whose
/// tallies changed, each weight, by class and feature, and what each message was taught as, by
/// Message-ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    pub removed: HashSet<String>,
    pub classes: HashSet<String>,
    pub weights: HashMap<String, HashSet<u64>>,
    pub taught: HashSet<String>,
}

/// A model of which class a message is of, learned message by message.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub classes: Vec<Class>,
    /// For each feature, the classes with a weight for it, by index into `classes`, and the
    /// weight: one lookup for each of a message's features, however many classes there are.
    weights: HashMap<u64, Vec<(usize, Weight)>>,
    /// What each message it learned from was taught as, by Message-ID: what it's taught again
    /// only once that changes.
    pub taught: HashMap<String, String>,
}

impl PartialEq for Model {
    /// The same classes, in order, with the same weights, in any order, taught the same.
    fn eq(&self, other: &Self) -> bool {
        self.classes == other.classes
            && self.taught == other.taught
            && self.weights_by_class() == other.weights_by_class()
    }
}

impl Model {
    /// The class called `name`, if it was learned.
    #[must_use]
    pub fn class(&self, name: &str) -> Option<&Class> {
        self.classes.iter().find(|c| c.name == name)
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.classes.iter().position(|c| c.name == name)
    }

    /// Adds a class, as kept: its weights are set with [`Model::set_weight`].
    pub fn add_class(&mut self, class: Class) {
        self.classes.push(class);
    }

    /// What the class called `class` learned of `feature`, if anything.
    #[must_use]
    pub fn weight(&self, class: &str, feature: u64) -> Option<Weight> {
        let class = self.index(class)?;
        self.weights
            .get(&feature)?
            .iter()
            .find(|(c, _)| *c == class)
            .map(|(_, w)| *w)
    }

    /// Sets what the class called `class` learned of `feature`, as kept. Of no class, it's
    /// dropped.
    pub fn set_weight(&mut self, class: &str, feature: u64, weight: Weight) {
        let Some(class) = self.index(class) else {
            return;
        };
        let weights = self.weights.entry(feature).or_default();
        match weights.iter_mut().find(|(c, _)| *c == class) {
            Some((_, w)) => *w = weight,
            None => weights.push((class, weight)),
        }
    }

    /// Each class's weights, by feature, in the order of `classes`.
    #[must_use]
    pub fn weights_by_class(&self) -> Vec<HashMap<u64, Weight>> {
        let mut by_class = vec![HashMap::new(); self.classes.len()];
        for (&feature, weights) in &self.weights {
            for (class, weight) in weights {
                if let Some(of_it) = by_class.get_mut(*class) {
                    of_it.insert(feature, *weight);
                }
            }
        }
        by_class
    }

    /// How likely a message with `features` is to be of each class, in order, summing to 1.
    /// Empty when there's no class.
    pub(crate) fn chances(&self, features: &[u64]) -> Vec<f64> {
        let mut scores = vec![0.0_f64; self.classes.len()];
        for weights in features.iter().filter_map(|f| self.weights.get(f)) {
            for (class, weight) in weights {
                if let Some(score) = scores.get_mut(*class) {
                    *score += f64::from(weight.value());
                }
            }
        }
        let top = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let odds: Vec<f64> = scores.iter().map(|s| (s - top).exp()).collect();
        let sum: f64 = odds.iter().sum();
        odds.into_iter().map(|o| o / sum).collect()
    }

    /// The class a message with `features` is most likely of, and how likely, if any.
    pub(crate) fn best(&self, features: &[u64]) -> Option<(&Class, f64)> {
        best(&self.classes, &self.chances(features))
    }

    /// Learns that a message with `features`, which include [`BIAS`], is of the class `name`,
    /// adding it if it's new. What it would have answered first, if `sure` enough of it, is
    /// counted as right or not.
    pub(crate) fn learn(&mut self, name: &str, features: &[u64], sure: f64, touched: &mut Touched) {
        let mut chances = self.chances(features);
        if let Some((guess, _)) = best(&self.classes, &chances).filter(|(_, c)| *c >= sure) {
            let guess = guess.name.clone();
            if let Some(guessed) = self.classes.iter_mut().find(|c| c.name == guess) {
                guessed.guessed += 1;
                guessed.right += u32::from(guess == name);
                if guessed.guessed > GUESSES_KEPT {
                    guessed.guessed /= 2;
                    guessed.right /= 2;
                }
                touched.classes.insert(guess);
            }
        }
        let target = if let Some(target) = self.index(name) {
            target
        } else {
            self.classes.push(Class::new(name));
            chances = self.chances(features);
            self.classes.len() - 1
        };
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "a chance's precision past f32's doesn't matter to a gradient"
        )]
        let gradients: Vec<f32> = chances
            .iter()
            .enumerate()
            .map(|(c, chance)| (chance - f64::from(u8::from(c == target))) as f32)
            .collect();
        let mut learned: Vec<Vec<u64>> = vec![Vec::new(); self.classes.len()];
        let mut learn = |class: usize, feature: u64, weight: &mut Weight| {
            if let (Some(&gradient), Some(learned)) = (gradients.get(class), learned.get_mut(class))
            {
                weight.learn(gradient);
                learned.push(feature);
            }
        };
        for &feature in features {
            let weights = self.weights.entry(feature).or_default();
            let mut has = vec![false; gradients.len()];
            for (class, weight) in weights.iter_mut() {
                let gradient = gradients.get(*class).copied().unwrap_or_default();
                if *class == target || feature == BIAS || gradient.abs() >= MIN_GRADIENT {
                    learn(*class, feature, weight);
                }
                if let Some(has) = has.get_mut(*class) {
                    *has = true;
                }
            }
            // A class's own messages' features, and every class's bias, have a weight from the
            // first.
            for (class, _) in has.iter().enumerate().filter(|(_, has)| !**has) {
                if class == target || feature == BIAS {
                    let mut weight = Weight::default();
                    learn(class, feature, &mut weight);
                    weights.push((class, weight));
                }
            }
        }
        for (class, features) in self.classes.iter().zip(learned) {
            if !features.is_empty() {
                let kept = touched.weights.entry(class.name.clone()).or_default();
                kept.extend(features);
            }
        }
        if let Some(class) = self.classes.get_mut(target) {
            class.messages += 1;
            touched.classes.insert(class.name.clone());
        }
    }

    /// Forgets the classes `keep` says no to, and what was taught as them.
    pub(crate) fn retain(&mut self, keep: impl Fn(&str) -> bool, touched: &mut Touched) {
        let mut moved = Vec::with_capacity(self.classes.len());
        let mut kept = 0;
        for class in &self.classes {
            if keep(&class.name) {
                moved.push(Some(kept));
                kept += 1;
            } else {
                moved.push(None);
                touched.removed.insert(class.name.clone());
                touched.classes.remove(&class.name);
                touched.weights.remove(&class.name);
            }
        }
        if kept == self.classes.len() {
            return;
        }
        self.classes.retain(|c| keep(&c.name));
        self.weights.retain(|_, weights| {
            weights.retain_mut(|(class, _)| {
                moved.get(*class).copied().flatten().is_some_and(|to| {
                    *class = to;
                    true
                })
            });
            !weights.is_empty()
        });
        self.taught.retain(|id, class| {
            let kept = keep(class);
            if !kept {
                touched.taught.remove(id);
            }
            kept
        });
    }
}

/// The likeliest of `classes`, by their `chances`, and how likely.
fn best<'a>(classes: &'a [Class], chances: &[f64]) -> Option<(&'a Class, f64)> {
    classes
        .iter()
        .zip(chances.iter().copied())
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learn(model: &mut Model, name: &str, features: &[u64]) {
        model.learn(name, features, 2.0, &mut Touched::default());
    }

    #[test]
    fn it_tells_classes_apart_by_their_features() {
        let mut model = Model::default();
        for _ in 0..10 {
            learn(&mut model, "a", &[BIAS, 1, 2]);
            learn(&mut model, "b", &[BIAS, 3, 4]);
        }
        let (best, chance) = model.best(&[BIAS, 1, 2]).unwrap();
        assert_eq!(best.name, "a");
        assert!(chance > 0.9, "sure: {chance}");
        let (best, _) = model.best(&[BIAS, 3]).unwrap();
        assert_eq!(best.name, "b");
        let sum: f64 = model.chances(&[BIAS, 3]).iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "chances sum to 1: {sum}");
    }

    #[test]
    fn a_class_keeps_weights_only_for_features_it_saw() {
        let mut model = Model::default();
        for _ in 0..5 {
            learn(&mut model, "a", &[BIAS, 1]);
            learn(&mut model, "b", &[BIAS, 2]);
        }
        assert!(model.weight("a", 1).is_some() && model.weight("a", BIAS).is_some());
        assert!(model.weight("a", 2).is_none(), "never saw 2");
        assert_eq!(model.class("a").unwrap().messages, 5);
    }

    #[test]
    fn a_feature_a_class_saw_learns_against_it_too() {
        let mut model = Model::default();
        learn(&mut model, "a", &[BIAS, 1]);
        for _ in 0..10 {
            learn(&mut model, "b", &[BIAS, 1]);
        }
        let (best, _) = model.best(&[BIAS, 1]).unwrap();
        assert_eq!(best.name, "b", "1 turned out to be b's");
        assert!(
            model.weight("a", 1).unwrap().value() < 0.0,
            "and counts against a"
        );
    }

    #[test]
    fn what_a_class_was_guessed_and_got_right_is_counted() {
        let mut model = Model::default();
        let mut touched = Touched::default();
        // Sure of anything: there's only ever one answer, a.
        model.learn("a", &[BIAS], 0.0, &mut touched);
        for _ in 0..3 {
            model.learn("a", &[BIAS], 0.0, &mut touched);
        }
        let a = model.class("a").unwrap();
        assert!(a.is_trusted(), "right 3 times of 3: {a:?}");
        // Before b weighs as much as a, a is still the answer, and wrong.
        for _ in 0..3 {
            model.learn("b", &[BIAS], 0.0, &mut touched);
        }
        let a = model.class("a").unwrap();
        assert!(!a.is_trusted(), "right 3 times of 6: {a:?}");
        assert!(touched.classes.contains("a") && touched.classes.contains("b"));
    }

    #[test]
    fn a_class_forgotten_takes_what_was_taught_as_it() {
        let mut model = Model::default();
        let mut touched = Touched::default();
        model.learn("a", &[BIAS, 1], 2.0, &mut touched);
        model.learn("b", &[BIAS, 2], 2.0, &mut touched);
        model.taught.insert("1@x".into(), "a".into());
        model.retain(|c| c != "a", &mut touched);
        assert!(model.class("a").is_none());
        assert!(model.taught.is_empty());
        assert!(touched.removed.contains("a"));
        assert!(touched.weights.keys().all(|c| c == "b"));
        assert!(model.weight("b", 2).is_some(), "b's weights are still b's");
        assert!(model.weight("a", 1).is_none());
    }

    #[test]
    fn nothing_learned_says_nothing() {
        assert!(Model::default().best(&[BIAS]).is_none());
    }
}
