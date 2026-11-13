//! Stable keypad projection, independent of intake and routing.
use crate::model::{Session, SESSION_KEYS_PER_PAGE};
use std::collections::HashSet;
#[derive(Default)]
pub struct KeypadLayout {
    positions: Vec<Option<String>>,
    old_idle: HashSet<String>,
    displaced: HashSet<String>,
}
impl KeypadLayout {
    /// Freeze current bright neighbours while retaining displaced dim tiles for later resumption.
    pub fn keep_positions(&mut self, acknowledged: &HashSet<String>) {
        self.displaced.retain(|key| acknowledged.contains(key));
    }

    pub fn remap(&mut self, old: &str, new: &str) {
        let exists = self
            .positions
            .iter()
            .any(|slot| slot.as_deref() == Some(new));
        for slot in &mut self.positions {
            if slot.as_deref() == Some(old) {
                *slot = if exists { None } else { Some(new.to_string()) };
            }
        }
        for keys in [&mut self.old_idle, &mut self.displaced] {
            if keys.remove(old) {
                keys.insert(new.to_string());
            }
        }
    }
    pub fn project(
        &mut self,
        recent: &[&Session],
        old_idle: &[&Session],
    ) -> Vec<Option<String>> {
        let positions = &mut self.positions;
        let previous_old_idle = &self.old_idle;
        // Keep ordinary sessions in place; expired, disabled, and old
        // idle sessions on page one vacate their positions without shifting neighbours.
        for (index, slot) in positions.iter_mut().enumerate() {
            if let Some(key) = slot.as_ref() {
                let keep = (recent.iter().any(|s| s.key() == *key && (s.acknowledged || !previous_old_idle.contains(key))))
                    || (index >= SESSION_KEYS_PER_PAGE && old_idle.iter().any(|s| s.key() == *key));
                if !keep {
                    *slot = None;
                }
            }
        }
        self.displaced.retain(|key| recent.iter().chain(old_idle).any(|s| s.key() == *key));
        // A new session or a resumed displaced session takes a free first-page key,
        // then an acknowledged key. Existing bright neighbours never move.
        for session in recent.iter().filter(|s| !s.acknowledged) {
            let key = session.key();
            let current = positions.iter().position(|slot| slot.as_ref() == Some(&key));
            if current.is_some() && !self.displaced.contains(&key) { continue; }
            let free = (0..SESSION_KEYS_PER_PAGE).find(|&i| positions.get(i).is_none_or(Option::is_none));
            let replace = free.or_else(|| (0..SESSION_KEYS_PER_PAGE).find(|&i| {
                positions.get(i).and_then(Option::as_ref).is_some_and(|k| recent.iter().any(|s| s.acknowledged && s.key() == *k))
            }));
            if let Some(index) = replace {
                if positions.len() <= index { positions.resize(index + 1, None); }
                let displaced = positions[index].take();
                if let Some(old_index) = current { positions[old_index] = None; }
                positions[index] = Some(key.clone());
                self.displaced.remove(&key);
                if let Some(old_key) = displaced {
                    let tail = (SESSION_KEYS_PER_PAGE..positions.len()).find(|&i| positions[i].is_none()).unwrap_or(positions.len().max(SESSION_KEYS_PER_PAGE));
                    if positions.len() <= tail { positions.resize(tail + 1, None); }
                    positions[tail] = Some(old_key.clone());
                    self.displaced.insert(old_key);
                }
            }
        }
        for (group, start) in [(recent, 0), (old_idle, SESSION_KEYS_PER_PAGE)] {
            for session in group {
                let key = session.key();
                if positions.iter().any(|slot| slot.as_ref() == Some(&key)) { continue; }
                let index = (start..positions.len()).find(|&i| positions[i].is_none()).unwrap_or(positions.len().max(start));
                if positions.len() <= index { positions.resize(index + 1, None); }
                positions[index] = Some(key);
            }
        }
        self.old_idle = old_idle.iter().map(|s| s.key()).collect();
        while positions.last().is_some_and(Option::is_none) {
            positions.pop();
        }
        positions.clone()
    }
}

/// Stable compact selection. Candidates already waiting off-screen cannot evict a
/// tile when it is acknowledged; newly arriving or resumed work can reuse it.
#[derive(Default)]
pub struct IslandLayout {
    slots: [Option<String>; 2],
    known: std::collections::HashMap<String, (bool, bool)>,
}
impl IslandLayout {
    pub fn remap(&mut self, old: &str, new: &str) {
        let exists = self.slots.iter().any(|s| s.as_deref() == Some(new));
        for slot in &mut self.slots {
            if slot.as_deref() == Some(old) { *slot = (!exists).then(|| new.to_owned()); }
        }
        if let Some(ack) = self.known.remove(old) { self.known.entry(new.to_owned()).or_insert(ack); }
    }

    pub fn project<'a>(&mut self, candidates: &[&'a Session]) -> Vec<&'a Session> {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|key| !candidates.iter().any(|s| s.key() == *key)) { *slot = None; }
        }
        let initial = self.known.is_empty();
        for session in candidates {
            let key = session.key();
            if self.slots.iter().any(|slot| slot.as_ref() == Some(&key)) { continue; }
            let attention = session.activity.group() == crate::model::Group::Waiting || session.pending_attention || session.unread > 0;
            let newly_eligible = self.known.get(&key).is_none_or(|&(ack, was_attention)|
                (ack && !session.acknowledged) || (!was_attention && attention));
            if let Some(index) = self.slots.iter().position(Option::is_none) {
                self.slots[index] = Some(key);
            } else if !initial && newly_eligible && !session.acknowledged {
                if let Some(index) = self.slots.iter().position(|slot| slot.as_ref().is_some_and(|key| candidates.iter().any(|s| s.key() == *key && s.acknowledged))) {
                    self.slots[index] = Some(key);
                }
            }
        }
        self.known = candidates.iter().map(|s| (s.key(), (s.acknowledged, s.activity.group() == crate::model::Group::Waiting || s.pending_attention || s.unread > 0))).collect();
        let mut ordered: Vec<_> = self.slots.iter().filter_map(|slot| slot.as_ref().and_then(|key| candidates.iter().find(|s| s.key() == *key).copied())).collect();
        ordered.extend(candidates.iter().copied().filter(|s| !self.slots.iter().any(|slot| slot.as_ref().is_some_and(|key| s.key() == *key))));
        ordered
    }
}
