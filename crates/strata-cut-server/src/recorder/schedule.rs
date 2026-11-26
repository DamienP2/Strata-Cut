//! Programmed recordings: a recorder in "schedule" mode records only during its slots
//! (one day, or days of the week, from a time to a time, in the server's time zone),
//! and keeps what it recorded for some days instead of a rolling loop.
use jiff::{civil, tz::TimeZone, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Mutex,
};

/// A slot of a recorder's programme.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    #[serde(default)]
    pub id: String,
    pub recorder: String,
    #[serde(default)]
    pub name: String,
    /// One day (YYYY-MM-DD); empty when the slot repeats.
    #[serde(default)]
    pub date: String,
    /// Repeated on these days of the week: 1 = Monday … 7 = Sunday.
    #[serde(default)]
    pub days: Vec<u8>,
    /// "HH:MM"; an end before the start means the next day.
    pub start: String,
    pub end: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Who programmed it (the account's id; empty for one programmed before this was kept).
    #[serde(default)]
    pub by: String,
}

fn yes() -> bool {
    true
}

fn hm(s: &str) -> Option<(i8, i8)> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

impl Slot {
    pub fn normalize(&mut self) {
        self.name = self.name.trim().chars().take(120).collect();
        self.date = self.date.trim().to_string();
        self.days.sort_unstable();
        self.days.dedup();
        if !self.date.is_empty() {
            self.days.clear();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.recorder.is_empty() {
            return Err("choose the recorder".into());
        }
        let (Some(s), Some(e)) = (hm(&self.start), hm(&self.end)) else { return Err("the times are written hh:mm".into()) };
        if s == e {
            return Err("the end is the same as the start".into());
        }
        if self.date.is_empty() {
            if self.days.is_empty() {
                return Err("choose a day".into());
            }
            if self.days.iter().any(|d| !(1..=7).contains(d)) {
                return Err("choose a day".into());
            }
        } else if self.date.parse::<civil::Date>().is_err() {
            return Err("invalid date".into());
        }
        Ok(())
    }

    /// The occurrences that record, overlapping [from, to) in ms (none while paused).
    pub fn occurrences(&self, tz: &TimeZone, from: i64, to: i64) -> Vec<(i64, i64)> {
        if self.enabled { self.times(tz, from, to) } else { Vec::new() }
    }

    /// Its times overlapping [from, to) in ms, in the time zone `tz`, paused or not (the interface).
    pub fn times(&self, tz: &TimeZone, from: i64, to: i64) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        let (Some((sh, sm)), Some((eh, em))) = (hm(&self.start), hm(&self.end)) else { return out };
        let day = |ms: i64| Timestamp::from_millisecond(ms).ok().map(|t| t.to_zoned(tz.clone()).date());
        let (Some(mut d), Some(last)) = (day(from - 86_400_000), day(to)) else { return out };
        let once = self.date.parse::<civil::Date>().ok();
        let ms = |d: civil::Date, h: i8, m: i8| d.at(h, m, 0, 0).to_zoned(tz.clone()).ok().map(|z| z.timestamp().as_millisecond());
        while d <= last {
            let on = match once {
                Some(o) => d == o,
                None => self.days.contains(&(d.weekday().to_monday_one_offset() as u8)),
            };
            if on {
                let end_day = if (eh, em) <= (sh, sm) { d.tomorrow().ok() } else { Some(d) };
                if let (Some(s), Some(e)) = (ms(d, sh, sm), end_day.and_then(|ed| ms(ed, eh, em))) {
                    if e > from && s < to {
                        out.push((s, e));
                    }
                }
            }
            let Ok(next) = d.tomorrow() else { break };
            d = next;
        }
        out
    }
}

/// The slots of every recorder (`schedules.json`).
pub struct Schedules {
    path: PathBuf,
    slots: Mutex<Vec<Slot>>,
    /// Wakes the recorders waiting for their next slot.
    pub changed: tokio::sync::Notify,
}

impl Schedules {
    pub fn load(path: PathBuf) -> Schedules {
        let slots = crate::paths::read_json(&path).unwrap_or_default();
        Schedules { path, slots: Mutex::new(slots), changed: tokio::sync::Notify::new() }
    }

    fn save(&self, slots: &[Slot]) -> Result<(), String> {
        crate::paths::write_json(&self.path, slots, false).map_err(|e| e.to_string())
    }

    pub fn all(&self) -> Vec<Slot> {
        self.slots.lock().unwrap().clone()
    }

    pub fn of(&self, recorder: &str) -> Vec<Slot> {
        self.slots.lock().unwrap().iter().filter(|s| s.recorder == recorder).cloned().collect()
    }

    /// Adds or replaces a slot (by id); `check` may refuse the new programme.
    pub fn put(&self, mut slot: Slot, check: impl FnOnce(&Schedules) -> Result<(), String>) -> Result<Slot, String> {
        slot.normalize();
        slot.validate()?;
        if slot.id.is_empty() {
            slot.id = crate::api::new_id();
        }
        let before = self.all();
        {
            let mut slots = self.slots.lock().unwrap();
            match slots.iter_mut().find(|s| s.id == slot.id) {
                Some(s) => *s = slot.clone(),
                None => slots.push(slot.clone()),
            }
        }
        if let Err(e) = check(self) {
            *self.slots.lock().unwrap() = before;
            return Err(e);
        }
        self.save(&self.all())?;
        self.changed.notify_waiters();
        Ok(slot)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut slots = self.slots.lock().unwrap();
        let n = slots.len();
        slots.retain(|s| s.id != id);
        if slots.len() == n {
            return Err("no such slot".into());
        }
        let copy = slots.clone();
        drop(slots);
        self.save(&copy)?;
        self.changed.notify_waiters();
        Ok(())
    }

    pub fn delete_recorder(&self, recorder: &str) {
        let mut slots = self.slots.lock().unwrap();
        slots.retain(|s| s.recorder != recorder);
        let copy = slots.clone();
        drop(slots);
        let _ = self.save(&copy);
        self.changed.notify_waiters();
    }

    /// The occurrence holding `now` for a recorder.
    pub fn active(&self, recorder: &str, tz: &TimeZone, now: i64) -> Option<(Slot, i64, i64)> {
        self.of(recorder).into_iter().find_map(|s| s.occurrences(tz, now, now + 1).into_iter().find(|&(a, b)| a <= now && now < b).map(|(a, b)| (s, a, b)))
    }

    /// The next occurrence starting after `now` (within 8 days).
    pub fn next(&self, recorder: &str, tz: &TimeZone, now: i64) -> Option<(Slot, i64, i64)> {
        self.of(recorder).into_iter().flat_map(|s| s.occurrences(tz, now, now + 8 * 86_400_000).into_iter().filter(|&(a, _)| a > now).map(move |(a, b)| (s.clone(), a, b))).min_by_key(|o| o.1)
    }

    /// The slots for the interface, each with its next (or current) occurrence.
    pub fn json(&self, tz: &TimeZone, now: i64) -> Value {
        let slots: Vec<Value> = self
            .all()
            .into_iter()
            .map(|s| {
                let occ = s.times(tz, now, now + 8 * 86_400_000).into_iter().find(|&(_, b)| b > now);
                let mut v = serde_json::to_value(&s).unwrap_or_default();
                v["next"] = occ.map(|(a, b)| json!({"start": a, "end": b})).unwrap_or(Value::Null);
                v["active"] = json!(s.enabled && occ.is_some_and(|(a, _)| a <= now));
                v
            })
            .collect();
        json!(slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str, tz: &TimeZone) -> i64 {
        s.parse::<civil::DateTime>().unwrap().to_zoned(tz.clone()).unwrap().timestamp().as_millisecond()
    }

    #[test]
    fn occurrences() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        // Mondays and Wednesdays 20:00 → 21:30; 2026-11-16 is a Monday.
        let s = Slot { id: "a".into(), recorder: "r".into(), name: "JT".into(), date: String::new(), days: vec![1, 3], start: "20:00".into(), end: "21:30".into(), enabled: true, by: String::new() };
        let o = s.occurrences(&tz, ms("2026-11-16T00:00", &tz), ms("2026-11-23T00:00", &tz));
        assert_eq!(o, vec![(ms("2026-11-16T20:00", &tz), ms("2026-11-16T21:30", &tz)), (ms("2026-11-18T20:00", &tz), ms("2026-11-18T21:30", &tz))]);
        // Over midnight, once.
        let n = Slot { date: "2026-11-20".into(), days: vec![], start: "23:30".into(), end: "01:00".into(), ..s.clone() };
        assert_eq!(n.occurrences(&tz, ms("2026-11-21T00:30", &tz), ms("2026-11-21T00:31", &tz)), vec![(ms("2026-11-20T23:30", &tz), ms("2026-11-21T01:00", &tz))]);
        assert!(n.validate().is_ok());
        assert!(Slot { days: vec![], ..s.clone() }.validate().is_err());
        assert!(Slot { end: "20:00".into(), ..s.clone() }.validate().is_err());
    }

    #[test]
    fn next_and_active() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        let dir = std::env::temp_dir().join(format!("sc-sched-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sch = Schedules::load(dir.join("s.json"));
        let s = Slot { id: String::new(), recorder: "r".into(), name: String::new(), date: String::new(), days: vec![1, 2, 3, 4, 5, 6, 7], start: "20:00".into(), end: "21:00".into(), enabled: true, by: String::new() };
        sch.put(s.clone(), |_| Ok(())).unwrap();
        // A second slot for the same recorder.
        sch.put(Slot { start: "20:30".into(), end: "21:00".into(), ..s }, |_| Ok(())).unwrap();
        let now = ms("2026-11-16T12:00", &tz);
        assert_eq!(sch.next("r", &tz, now).map(|n| n.1), Some(ms("2026-11-16T20:00", &tz)));
        assert!(sch.active("r", &tz, ms("2026-11-16T20:40", &tz)).is_some());
        assert!(sch.active("r", &tz, ms("2026-11-16T21:00", &tz)).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
