//! Command indices need more than one byte: the registry has hundreds of entries.

pub fn parse(line: &[u8], commands: usize) -> Option<(usize, &[u8])> {
    if commands == 0 {
        return None;
    }
    let (&lo, rest) = line.split_first()?;
    let (&hi, json) = rest.split_first()?;
    Some((usize::from(u16::from_le_bytes([lo, hi])) % commands, json))
}

#[cfg(test)]
pub fn all_reachable(commands: usize) -> bool {
    let mut seen = vec![false; commands];
    for value in 0..=u16::MAX {
        let line = value.to_le_bytes();
        // The outer target splits lines at LF, so selectors containing LF cannot be used.
        if line.contains(&b'\n') {
            continue;
        }
        if let Some((index, _)) = parse(&line, commands) {
            seen[index] = true;
        }
    }
    !seen.is_empty() && seen.into_iter().all(|reached| reached)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reaches_commands_after_the_first_256_without_consuming_json() {
        assert_eq!(parse(b"\x00\x01{}", 487), Some((256, b"{}".as_slice())));
        assert!(all_reachable(487));
        assert!(parse(&[], 487).is_none());
        assert!(parse(&[0], 487).is_none());
        assert!(parse(&[0, 0], 0).is_none());
    }
}
