//! Month-grid math for the calendar popover (E2.5): which dates fill the 7-column grid of a month.

use chrono::{Datelike, Days, NaiveDate, Weekday};

/// The grid of `month` (1–12) of `year`, one row per week starting on `week_start`: leading and trailing days come
/// from the neighbouring months (drawn at `label-tertiary`), and only as many rows as the month needs (4–6).
/// `None` for an invalid month or a year outside chrono's range.
pub fn month_grid(year: i32, month: u32, week_start: Weekday) -> Option<Vec<[NaiveDate; 7]>> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let lead = first.weekday().days_since(week_start);
    let len = u32::from(first.num_days_in_month());
    let start = first.checked_sub_days(Days::new(lead.into()))?;
    let rows = (lead + len).div_ceil(7);
    (0..rows)
        .map(|row| {
            let mut week = [start; 7];
            for (i, day) in week.iter_mut().enumerate() {
                *day = start.checked_add_days(Days::new((row * 7 + i as u32).into()))?;
            }
            Some(week)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn in_month(grid: &[[NaiveDate; 7]], month: u32) -> usize {
        grid.iter().flatten().filter(|d| d.month() == month).count()
    }

    #[test]
    fn october_2026_sunday_start() {
        let grid = month_grid(2026, 10, Weekday::Sun).unwrap();
        assert_eq!(grid.len(), 5);
        assert_eq!(grid[0][0], date(2026, 9, 27));
        assert_eq!(grid[0][4], date(2026, 10, 1));
        // The 17th is a Saturday: last column of the third row.
        assert_eq!(grid[2][6], date(2026, 10, 17));
        assert_eq!(grid[4][6], date(2026, 10, 31));
    }

    #[test]
    fn leap_years() {
        // 2028 is a leap year: 29 days, from Tuesday the 1st.
        let grid = month_grid(2028, 2, Weekday::Mon).unwrap();
        assert_eq!((grid.len(), in_month(&grid, 2)), (5, 29));
        assert_eq!(grid[0][0], date(2028, 1, 31));
        assert_eq!(grid[4][1], date(2028, 2, 29));
        // Divisible by 100 but not 400: not a leap year. By 400: a leap year.
        assert_eq!(in_month(&month_grid(2100, 2, Weekday::Mon).unwrap(), 2), 28);
        assert_eq!(in_month(&month_grid(2000, 2, Weekday::Mon).unwrap(), 2), 29);
    }

    #[test]
    fn row_count_follows_the_month() {
        // February 2027 starts on a Monday and has 28 days: exactly four rows with a Monday start, five with Sunday.
        assert_eq!(month_grid(2027, 2, Weekday::Mon).unwrap().len(), 4);
        assert_eq!(month_grid(2027, 2, Weekday::Sun).unwrap().len(), 5);
        // August 2026 starts on a Saturday and has 31 days: six rows with a Sunday start.
        assert_eq!(month_grid(2026, 8, Weekday::Sun).unwrap().len(), 6);
    }

    #[test]
    fn every_month_is_contiguous() {
        for year in [1999, 2024, 2026, 2100] {
            for month in 1..=12 {
                for start in [Weekday::Sun, Weekday::Mon, Weekday::Sat] {
                    let grid = month_grid(year, month, start).unwrap();
                    let days: Vec<_> = grid.iter().flatten().collect();
                    assert!(days.windows(2).all(|w| w[0].succ_opt() == Some(*w[1])));
                    assert!(grid.iter().all(|row| row[0].weekday() == start));
                    assert_eq!(in_month(&grid, month), usize::from(date(year, month, 1).num_days_in_month()));
                }
            }
        }
    }

    #[test]
    fn week_numbers_are_iso() {
        // Week numbers in the grid are `NaiveDate::iso_week` of each row's first day: 2026 has 53 ISO weeks.
        assert_eq!(date(2027, 1, 1).iso_week().week(), 53);
        assert_eq!(date(2027, 2, 1).iso_week().week(), 5);
    }

    #[test]
    fn invalid_input() {
        assert!(month_grid(2026, 0, Weekday::Mon).is_none());
        assert!(month_grid(2026, 13, Weekday::Mon).is_none());
    }
}
