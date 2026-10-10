//! Strict, immutable Accounts UUID backfill export. This is data migration only.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub new_uuid: String,
    pub kind: String,
}
pub type Mapping = BTreeMap<String, Link>;

pub fn parse(text: &str) -> Result<Mapping, String> {
    let mut lines = text.lines();
    if lines.next() != Some("old_uuid,new_uuid,kind") {
        return Err("Expected exact CSV header old_uuid,new_uuid,kind".into());
    }
    let mut result = Mapping::new();
    let mut targets = BTreeSet::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split(',').collect();
        if fields.len() != 3 {
            return Err("Expected three CSV fields".into());
        }
        let (old, new, kind) = (fields[0], fields[1], fields[2]);
        let canonical = |s: &str| {
            s.len() == 36
                && s.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                    }
                })
        };
        if !(canonical(old)
            || (!old.is_empty()
                && old.len() <= 64
                && old.bytes().all(|b| b.is_ascii_alphanumeric())))
            || !canonical(new)
            || new.as_bytes()[14] != b'4'
            || !b"89ab".contains(&new.as_bytes()[19])
            || !["carbon", "silicon"].contains(&kind)
            || old == new
        {
            return Err(
                "Expected valid source, canonical lowercase UUIDv4 target and carbon|silicon kind"
                    .into(),
            );
        }
        if result
            .insert(
                old.into(),
                Link {
                    new_uuid: new.into(),
                    kind: kind.into(),
                },
            )
            .is_some()
            || !targets.insert(new.to_owned())
        {
            return Err("Duplicate source or target; merging accounts is forbidden".into());
        }
    }
    if result.is_empty() || result.keys().any(|old| targets.contains(old)) {
        return Err("Empty, chained or cyclic mappings are forbidden".into());
    }
    Ok(result)
}

/// Validate immutable replay history and select rows not previously applied.
pub fn fresh(mapping: &Mapping, ledger: &Mapping) -> Result<Mapping, String> {
    for (old, link) in mapping {
        if ledger.get(old).is_some_and(|saved| saved != link)
            || ledger.iter().any(|(source, saved)| {
                source != old
                    && (saved.new_uuid == link.new_uuid
                        || saved.new_uuid == *old
                        || link.new_uuid == *source)
            })
        {
            return Err("Mapping conflicts with immutable migration history".into());
        }
    }
    Ok(mapping
        .iter()
        .filter(|(old, _)| !ledger.contains_key(*old))
        .map(|(old, link)| (old.clone(), link.clone()))
        .collect())
}

pub fn mapped<'a>(mapping: &'a Mapping, value: &'a str) -> &'a str {
    mapping
        .get(value)
        .map_or(value, |link| link.new_uuid.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_mapping_and_immutable_replay() {
        let text = "old_uuid,new_uuid,kind\nAda,f858d0b5-98ba-4a4d-8ce5-114e93136f23,carbon\n";
        let m = parse(text).unwrap();
        assert!(fresh(&m, &m).unwrap().is_empty());
        for bad in [
            text.replace("old_uuid", "old"),
            text.replace("4a4d", "1a4d"),
            text.replace("f858", "F858"),
            format!("{text}Bob,f858d0b5-98ba-4a4d-8ce5-114e93136f23,carbon\n"),
        ] {
            assert!(parse(&bad).is_err());
        }
        assert!(fresh(&parse(&text.replace("carbon", "silicon")).unwrap(), &m).is_err());
    }
}
