//! news-lens domain crate
//!
//! This crate contains the core domain logic following hexagonal architecture:
//! - `model`: Domain entities and value objects
//! - `ports`: Trait definitions for external dependencies (adapters)
//! - `usecases`: Application use cases / business logic

pub mod model;
pub mod ports;
pub mod usecases;

pub use model::*;
pub use ports::*;

use std::cmp::Ordering;

/// Compare post IDs while handling decimal numeric IDs safely.
///
/// If both IDs contain only ASCII digits, compares by numeric value without parsing
/// (length first, then lexicographic), which works for arbitrarily large values.
/// Otherwise falls back to plain lexicographic comparison.
pub fn compare_post_ids(a: &str, b: &str) -> Ordering {
    let a_is_digits = !a.is_empty() && a.as_bytes().iter().all(|byte| byte.is_ascii_digit());
    let b_is_digits = !b.is_empty() && b.as_bytes().iter().all(|byte| byte.is_ascii_digit());

    if a_is_digits && b_is_digits {
        let a_norm = normalize_numeric_id(a);
        let b_norm = normalize_numeric_id(b);
        return a_norm
            .len()
            .cmp(&b_norm.len())
            .then_with(|| a_norm.cmp(b_norm));
    }

    a.cmp(b)
}

fn normalize_numeric_id(id: &str) -> &str {
    let trimmed = id.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_numeric_ids_by_value() {
        assert_eq!(compare_post_ids("9", "10"), Ordering::Less);
        assert_eq!(compare_post_ids("99", "100"), Ordering::Less);
        assert_eq!(compare_post_ids("123", "45"), Ordering::Greater);
    }

    #[test]
    fn compare_numeric_ids_with_leading_zeros() {
        assert_eq!(compare_post_ids("0009", "10"), Ordering::Less);
        assert_eq!(compare_post_ids("0010", "10"), Ordering::Equal);
        assert_eq!(compare_post_ids("0", "0000"), Ordering::Equal);
    }

    #[test]
    fn compare_non_numeric_ids_lexicographically() {
        assert_eq!(compare_post_ids("tweet1", "tweet2"), Ordering::Less);
        assert_eq!(compare_post_ids("abc", "100"), Ordering::Greater);
        assert_eq!(compare_post_ids("", "0"), Ordering::Less);
        assert_eq!(compare_post_ids("0", ""), Ordering::Greater);
    }
}
