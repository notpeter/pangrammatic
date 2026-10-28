//! Maps character offsets in the text Pangram returns back to offsets in the
//! text we sent.
//!
//! Pangram 4 may normalize the submitted text, and window offsets refer to
//! the normalized `text` in the response. Whitespace is the usual casualty,
//! so the alignment ignores whitespace entirely and diffs the remaining
//! characters, which also survives small substitutions.

use similar::{capture_diff_slices, Algorithm, DiffTag};

#[derive(Debug)]
pub struct Alignment {
    /// For each char index of the remote text, the matching local char index.
    remote_to_local: Vec<Option<usize>>,
}

impl Alignment {
    pub fn new(local: &[char], remote: &[char]) -> Self {
        let mut remote_to_local = vec![None; remote.len()];

        if local == remote {
            for (i, slot) in remote_to_local.iter_mut().enumerate() {
                *slot = Some(i);
            }
            return Self { remote_to_local };
        }

        let local_idx: Vec<usize> = (0..local.len())
            .filter(|&i| !local[i].is_whitespace())
            .collect();
        let remote_idx: Vec<usize> = (0..remote.len())
            .filter(|&i| !remote[i].is_whitespace())
            .collect();
        let local_nw: Vec<char> = local_idx.iter().map(|&i| local[i]).collect();
        let remote_nw: Vec<char> = remote_idx.iter().map(|&i| remote[i]).collect();

        for op in capture_diff_slices(Algorithm::Myers, &local_nw, &remote_nw) {
            let (tag, old_range, new_range) = (op.tag(), op.old_range(), op.new_range());
            match tag {
                DiffTag::Equal => {
                    for (o, n) in old_range.zip(new_range) {
                        remote_to_local[remote_idx[n]] = Some(local_idx[o]);
                    }
                }
                DiffTag::Replace if old_range.len() == new_range.len() => {
                    // Same-length substitution (e.g. a normalized quote):
                    // keep positions aligned.
                    for (o, n) in old_range.zip(new_range) {
                        remote_to_local[remote_idx[n]] = Some(local_idx[o]);
                    }
                }
                DiffTag::Replace | DiffTag::Insert | DiffTag::Delete => {}
            }
        }

        // Whitespace in the remote text maps to the matching local position
        // when it sits between two mapped neighbours; otherwise it is left
        // unmapped (it has no glyph anyway).
        Self { remote_to_local }
    }

    /// Fraction of non-whitespace remote characters that found a local match.
    pub fn coverage(&self, remote: &[char]) -> f64 {
        let mut total = 0usize;
        let mut mapped = 0usize;
        for (i, c) in remote.iter().enumerate() {
            if c.is_whitespace() {
                continue;
            }
            total += 1;
            if self.remote_to_local[i].is_some() {
                mapped += 1;
            }
        }
        if total == 0 {
            1.0
        } else {
            mapped as f64 / total as f64
        }
    }

    /// Maps a half-open remote char range to the local char indices it covers,
    /// in order, skipping characters with no counterpart.
    pub fn local_indices(&self, start: usize, end: usize) -> Vec<usize> {
        let end = end.min(self.remote_to_local.len());
        if start >= end {
            return Vec::new();
        }
        self.remote_to_local[start..end]
            .iter()
            .filter_map(|m| *m)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn identical() {
        let a = chars("hello world");
        let al = Alignment::new(&a, &a);
        assert_eq!(al.local_indices(6, 11), vec![6, 7, 8, 9, 10]);
    }

    #[test]
    fn whitespace_collapsed() {
        let local = chars("hello\n\n  world  again");
        let remote = chars("hello world again");
        let al = Alignment::new(&local, &remote);
        assert_eq!(al.coverage(&remote), 1.0);
        assert_eq!(al.local_indices(6, 11), vec![9, 10, 11, 12, 13]);
    }

    #[test]
    fn remote_insertion_is_skipped() {
        let local = chars("abc def");
        let remote = chars("abc XYZ def");
        let al = Alignment::new(&local, &remote);
        assert_eq!(al.local_indices(4, 7), Vec::<usize>::new());
        assert_eq!(al.local_indices(8, 11), vec![4, 5, 6]);
    }
}
