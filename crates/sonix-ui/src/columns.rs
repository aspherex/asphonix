//! Which optional Tracks columns fit. A container query in the Svelte app
//! (`@container track-table (max-width: ...)`): the width is the table panel's
//! *content* box (border excluded), `max-width` is inclusive, so Added is hidden at
//! <= 760 px and Year at <= 600 px. Header and rows read the same value.

use crate::metrics::tracks::{HIDE_ADDED_BELOW as HIDE_ADDED_AT, HIDE_YEAR_BELOW as HIDE_YEAR_AT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub year: bool,
    pub added: bool,
}

pub fn visible_columns(content_width: f32) -> Columns {
    Columns { added: content_width > HIDE_ADDED_AT, year: content_width > HIDE_YEAR_AT }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_query_boundaries_are_inclusive() {
        assert_eq!(visible_columns(761.0), Columns { year: true, added: true });
        assert_eq!(visible_columns(760.0), Columns { year: true, added: false });
        assert_eq!(visible_columns(601.0), Columns { year: true, added: false });
        assert_eq!(visible_columns(600.0), Columns { year: false, added: false });
        assert_eq!(visible_columns(0.0), Columns { year: false, added: false });
        assert_eq!(visible_columns(2000.0), Columns { year: true, added: true });
    }
}
