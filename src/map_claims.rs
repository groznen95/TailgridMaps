// SPDX-License-Identifier: GPL-3.0-only
use crate::runtime::Dimension;
use std::{collections::BTreeMap, sync::Mutex, time::{Duration, Instant}};

pub type Chunk = (Dimension, i32, i32);
const RADIUS: i32 = 5;
const WIDTH: usize = 2 * RADIUS as usize + 1;
const RESPONSE_TIME: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    Own,
    Other(Option<String>),
}

impl Claim {
    pub fn is_own(&self) -> bool { matches!(self, Self::Own) }
}

fn origin(value: &str) -> Option<i32> {
    let n: i32 = value.parse().ok()?;
    (n.abs_diff(0) <= 30_000_000 && n % 16 == 0).then_some(n / 16)
}

pub fn own_chunks(line: &str) -> Option<Vec<Chunk>> {
    let (count, list) = line.strip_prefix("Your chunks (")?.split_once("): ")?;
    let count = count.split_once(" of ")?.0.parse::<usize>().ok()?;
    if count == 0 {
        return list.starts_with("none").then(Vec::new);
    }
    if count > 8192 {
        return None;
    }
    let chunks = list.split("; ").map(|entry| {
        let [dimension, "at", "x", x, "z", z]: [&str; 6] =
            entry.split_ascii_whitespace().collect::<Vec<_>>().try_into().ok()? else {
                return None;
            };
        let dimension = match dimension {
            "overworld" | "minecraft:overworld" => Dimension::Overworld,
            "the_nether" | "minecraft:the_nether" => Dimension::Nether,
            "the_end" | "minecraft:the_end" => Dimension::End,
            _ => return None,
        };
        Some((dimension, origin(x)?, origin(z)?))
    }).collect::<Option<Vec<_>>>()?;
    (chunks.len() == count).then_some(chunks)
}

fn bounds(range: &str) -> Option<[i32; 2]> {
    let range = range.strip_prefix("x ")?;
    let (x, z) = range.split_once(", z ")?;
    let chunk = |range: &str| {
        let (from, to) = range.split_once(" to ")?;
        let chunk = origin(from)?;
        (to.parse::<i32>().ok()? == chunk * 16 + 15).then_some(chunk)
    };
    Some([chunk(x)?, chunk(z)?])
}

pub fn here(line: &str) -> Option<([i32; 2], Option<&str>)> {
    let (range, owner) = line.strip_prefix("This chunk (")?.split_once(") is ")?;
    let owner = if owner == "free: /claim it." { None } else {
        let name = owner.strip_suffix("'s.")?;
        if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
            return None;
        }
        Some(name)
    };
    Some((bounds(range)?, owner))
}

fn claimed(line: &str) -> Option<[i32; 2]> {
    let (range, count) = line.strip_prefix("Claimed this chunk (")?.split_once("), ")?;
    let (held, limit) = count.strip_suffix(". /trust <player> lets a friend build.")?.split_once(" of ")?;
    let held = held.parse::<u32>().ok()?;
    let limit = limit.parse::<u32>().ok()?;
    (held > 0 && held <= limit).then_some(bounds(range)?)
}

fn released(line: &str) -> Option<[i32; 2]> {
    bounds(line.strip_prefix("Unclaimed this chunk (")?.strip_suffix(").")?)
}

pub enum Reply {
    Own(Vec<Chunk>),
    Here(Chunk, Option<String>),
    Changed(Chunk, bool),
    Nearby(Chunk, Vec<[u8; WIDTH]>),
}

pub fn apply(claims: &mut BTreeMap<Chunk, Claim>, reply: Reply) {
    match reply {
        Reply::Own(own) => {
            claims.retain(|_, claim| !claim.is_own());
            for chunk in own { claims.insert(chunk, Claim::Own); }
        }
        Reply::Here(chunk, Some(owner)) => {
            if !claims.get(&chunk).is_some_and(Claim::is_own) {
                claims.insert(chunk, Claim::Other(Some(owner)));
            }
        }
        Reply::Here(chunk, None) | Reply::Changed(chunk, false) => { claims.remove(&chunk); }
        Reply::Changed(chunk, true) => { claims.insert(chunk, Claim::Own); }
        Reply::Nearby((dimension, x, z), rows) => {
            for (dz, row) in rows.iter().enumerate() {
                for (dx, &symbol) in row.iter().enumerate() {
                    let chunk = (dimension, x + dx as i32 - RADIUS, z + dz as i32 - RADIUS);
                    match symbol {
                        b'-' => { claims.remove(&chunk); }
                        b'#' => { claims.insert(chunk, Claim::Own); }
                        b'+' | b'X' => { claims.insert(chunk, Claim::Other(None)); }
                        _ => {} // O gives the position, so /claim info supplies ownership here.
                    }
                }
            }
        }
    }
    if claims.len() > 8192 {
        let mut excess = claims.len() - 8192;
        claims.retain(|_, claim| {
            if excess == 0 || claim.is_own() { return true; }
            excess -= 1;
            false
        });
    }
}

struct Survey {
    center: Chunk,
    rows: Vec<[u8; WIDTH]>,
    started: Instant,
    silent: bool,
}

fn survey_center(line: &str) -> Option<[i32; 2]> {
    let (center, legend) = line.strip_prefix("Claims around chunk ")?.split_once(", ")?;
    if legend != "north up (O you, # yours, + trusts you, X someone else's, - free):" { return None; }
    let (x, z) = center.split_once(' ')?;
    let values = [x.parse::<i32>().ok()?, z.parse::<i32>().ok()?];
    values.iter().all(|v| v.unsigned_abs() <= 1_875_000).then_some(values)
}

struct Inbox {
    replies: Vec<Reply>,
    survey: Option<Survey>,
    expected_map: Option<(Dimension, Instant)>,
    expected_here: Option<(Dimension, Instant)>,
}

impl Inbox {
    const fn new() -> Self {
        Self { replies: Vec::new(), survey: None, expected_map: None, expected_here: None }
    }

    fn push(&mut self, reply: Reply) {
        if self.replies.len() < 32 { self.replies.push(reply); }
    }

    fn receive(&mut self, line: &str, dimension: Dimension) -> bool {
        if let Some([x, z]) = survey_center(line) {
            let silent = self.expected_map.take().is_some_and(|(d, sent)| d == dimension && sent.elapsed() < RESPONSE_TIME);
            self.survey = Some(Survey { center: (dimension, x, z), rows: Vec::with_capacity(WIDTH),
                started: Instant::now(), silent });
            return silent;
        }
        if let Some(survey) = &mut self.survey {
            if survey.center.0 != dimension || survey.started.elapsed() >= RESPONSE_TIME {
                self.survey = None;
            } else if let Ok(row) = <[u8; WIDTH]>::try_from(line.as_bytes())
                && row.iter().all(|ch| b"O#+X-".contains(ch))
            {
                let center_row = survey.rows.len() == RADIUS as usize;
                if row.iter().enumerate().any(|(i, &ch)| (ch == b'O') != (center_row && i == RADIUS as usize)) {
                    self.survey = None;
                    return false;
                }
                let silent = survey.silent;
                survey.rows.push(row);
                if survey.rows.len() == WIDTH {
                    let survey = self.survey.take().unwrap();
                    self.push(Reply::Nearby(survey.center, survey.rows));
                }
                return silent;
            }
        }
        if let Some(([x, z], owner)) = here(line) {
            self.push(Reply::Here((dimension, x, z), owner.map(str::to_owned)));
            return self.expected_here.take().is_some_and(|(d, sent)| d == dimension && sent.elapsed() < RESPONSE_TIME);
        }
        if let Some([x, z]) = claimed(line) {
            self.push(Reply::Changed((dimension, x, z), true));
        } else if let Some([x, z]) = released(line) {
            self.push(Reply::Changed((dimension, x, z), false));
        } else if let Some(chunks) = own_chunks(line) {
            self.push(Reply::Own(chunks));
        }
        false
    }
}

static INBOX: Mutex<Inbox> = Mutex::new(Inbox::new());

pub fn expect_survey(dimension: Dimension) {
    let mut inbox = INBOX.lock().unwrap_or_else(|e| e.into_inner());
    let request = Some((dimension, Instant::now()));
    inbox.expected_map = request;
    inbox.expected_here = request;
}

pub fn receive(line: &str, dimension: Dimension) -> bool {
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).receive(line, dimension)
}

pub fn take_replies() -> Vec<Reply> {
    std::mem::take(&mut INBOX.lock().unwrap_or_else(|e| e.into_inner()).replies)
}

pub fn reset() {
    *INBOX.lock().unwrap_or_else(|e| e.into_inner()) = Inbox::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smp_claim_messages_are_complete_and_aligned() {
        assert_eq!(
            own_chunks("Your chunks (2 of 8): overworld at x -16 z 32; the_nether at x 0 z -48"),
            Some(vec![(Dimension::Overworld, -1, 2), (Dimension::Nether, 0, -3)]),
        );
        assert_eq!(own_chunks("Your chunks (0 of 4): none, /claim where you stand"), Some(vec![]));
        assert_eq!(own_chunks("Your chunks (2 of 8): overworld at x -16 z 32"), None);
        assert_eq!(here("This chunk (x -16 to -1, z 32 to 47) is Alex's."), Some(([-1, 2], Some("Alex"))));
        assert_eq!(here("This chunk (x -16 to 0, z 32 to 47) is Alex's."), None);
    }

    #[test]
    fn claim_success_updates_the_chunk_immediately_and_unclaim_removes_it() {
        let mut inbox = Inbox::new();
        let mut claims = BTreeMap::new();
        let dimension = Dimension::Nether;
        let chunk = (dimension, -1, 2);
        claims.insert(chunk, Claim::Other(None));
        assert!(!inbox.receive("You hold 4 of 4 chunks: play longer for more, or /unclaim one.", dimension));
        assert!(inbox.replies.is_empty());
        assert!(!inbox.receive("Claimed this chunk (x -16 to -1, z 32 to 47), 1 of 4. /trust <player> lets a friend build.", dimension));
        for reply in inbox.replies.drain(..) { apply(&mut claims, reply); }
        assert_eq!(claims.get(&chunk), Some(&Claim::Own));
        assert!(!inbox.receive("Unclaimed this chunk (x -16 to -1, z 32 to 47).", dimension));
        for reply in inbox.replies.drain(..) { apply(&mut claims, reply); }
        assert!(claims.is_empty());
    }

    #[test]
    fn survey_places_foreign_claims_without_overwriting_the_current_chunk() {
        let mut inbox = Inbox::new();
        let mut claims = BTreeMap::new();
        let dimension = Dimension::Overworld;
        let center = (dimension, -304, -701);
        let west = (dimension, -305, -701);
        let north = (dimension, -304, -702);
        let previous = (dimension, -303, -700);
        claims.insert(center, Claim::Own);
        claims.insert(previous, Claim::Other(Some("Alex".into())));
        inbox.expected_map = Some((dimension, Instant::now()));
        assert!(inbox.receive("Claims around chunk -304 -701, north up (O you, # yours, + trusts you, X someone else's, - free):", dimension));
        for _ in 0..4 { assert!(inbox.receive("-----------", dimension)); }
        assert!(inbox.receive("-----+-----", dimension));
        assert!(inbox.receive("----XO-----", dimension));
        assert!(inbox.replies.is_empty());
        for _ in 0..5 { assert!(inbox.receive("-----------", dimension)); }
        for reply in inbox.replies.drain(..) { apply(&mut claims, reply); }
        assert_eq!(claims.get(&west), Some(&Claim::Other(None)));
        assert_eq!(claims.get(&north), Some(&Claim::Other(None)));
        assert_eq!(claims.get(&center), Some(&Claim::Own));
        assert!(!claims.contains_key(&previous));
        assert!(!inbox.receive("-----------", dimension));
    }
}
