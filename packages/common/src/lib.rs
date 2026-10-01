pub trait ByteSliceExt {
    fn find(&self, needle: &[u8]) -> Option<usize>;
}

impl ByteSliceExt for [u8] {
    fn find(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        self.windows(needle.len())
            .position(|window| window == needle)
    }
}

pub trait VecExt<T> {
    /// Equivalent to ".extend_from_slice(&parts.join(separator))" without intermediate allocation.
    fn extend_from_slices_with_separator(&mut self, parts: &[&[T]], separator: &[T]);
}

impl<T: Clone> VecExt<T> for Vec<T> {
    fn extend_from_slices_with_separator(&mut self, parts: &[&[T]], separator: &[T]) {
        let total: usize = parts.iter().map(|part| part.len()).sum::<usize>()
            + separator.len() * parts.len().saturating_sub(1);
        self.reserve(total);
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                self.extend_from_slice(separator);
            }
            self.extend_from_slice(part);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn separator_goes_only_between_parts() {
        let mut out = b"c=".to_vec();
        out.extend_from_slices_with_separator(&[b"IN", b"IP4", b"10.0.0.1"], b" ");
        assert_eq!(out, b"c=IN IP4 10.0.0.1");

        /* an empty part still gets its separators, so field positions are kept */
        let mut out = Vec::new();
        out.extend_from_slices_with_separator(&[b"a", b"", b"c"], b", ");
        assert_eq!(out, b"a, , c");

        let mut out = b"x".to_vec();
        out.extend_from_slices_with_separator(&[], b" ");
        assert_eq!(out, b"x");
    }
}
