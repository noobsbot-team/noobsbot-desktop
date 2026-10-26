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

/// The ISO week number of a grid row: the week its Monday falls in, whichever day the row starts on.
pub fn week_number(row: &[NaiveDate; 7]) -> u32 {
    row.iter().find(|d| d.weekday() == Weekday::Mon).unwrap_or(&row[0]).iso_week().week()
}

/// The locale's first weekday (LC_TIME), from `locale week-1stday first_weekday`; Sunday when it can't be read.
/// ponytail: read once per run; a locale change shows after a restart.
pub fn locale_week_start() -> Weekday {
    static START: std::sync::OnceLock<Weekday> = std::sync::OnceLock::new();
    *START.get_or_init(|| {
        let out = std::process::Command::new("locale").args(["week-1stday", "first_weekday"]).output();
        out.ok().and_then(|o| parse_first_weekday(&String::from_utf8_lossy(&o.stdout))).unwrap_or(Weekday::Sun)
    })
}

/// `week-1stday` is a date whose weekday is day 1 (19971130, a Sunday, in most locales); `first_weekday` counts from
/// it, starting at 1.
fn parse_first_weekday(out: &str) -> Option<Weekday> {
    let mut lines = out.lines().map(str::trim);
    let base = NaiveDate::parse_from_str(lines.next()?, "%Y%m%d").ok()?.weekday();
    let n: u8 = lines.next()?.parse().ok()?;
    (1..=7).contains(&n).then(|| (1..n).fold(base, |day, _| day.succ()))
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
        // 2026 starts on a Thursday, so it has 53 ISO weeks; New Year's Day 2027 (a Friday) is still in week 53.
        let weeks = |y, m, start| month_grid(y, m, start).unwrap().iter().map(week_number).collect::<Vec<_>>();
        for start in [Weekday::Sun, Weekday::Mon, Weekday::Sat] {
            let dec = month_grid(2026, 12, start).unwrap();
            let last = dec.iter().find(|row| row.contains(&date(2026, 12, 31))).unwrap();
            assert_eq!(week_number(last), 53);
            let jan = month_grid(2027, 1, start).unwrap();
            let first = jan.iter().find(|row| row.contains(&date(2027, 1, 1))).unwrap();
            assert_eq!(week_number(first), 53);
        }
        assert_eq!(weeks(2027, 1, Weekday::Mon), [53, 1, 2, 3, 4]);
        // A Sunday start: the row's Sunday belongs to the week before, its Monday decides.
        assert_eq!(weeks(2026, 10, Weekday::Sun), [40, 41, 42, 43, 44]);
        // 2020 (leap year from a Wednesday) also has 53 weeks; 2021 has 52.
        assert_eq!(week_number(&month_grid(2020, 12, Weekday::Mon).unwrap()[4]), 53);
        assert_eq!(week_number(&month_grid(2021, 12, Weekday::Mon).unwrap()[4]), 52);
    }

    #[test]
    fn locale_first_weekday() {
        // en_US: Sunday; de_DE: Monday (19971130 + 2 − 1); ar_EG-style: Saturday; a Monday-based base date.
        assert_eq!(parse_first_weekday("19971130\n1\n"), Some(Weekday::Sun));
        assert_eq!(parse_first_weekday("19971130\n2\n"), Some(Weekday::Mon));
        assert_eq!(parse_first_weekday("19971130\n7\n"), Some(Weekday::Sat));
        assert_eq!(parse_first_weekday("19971201\n1\n"), Some(Weekday::Mon));
        assert_eq!(parse_first_weekday(""), None);
        assert_eq!(parse_first_weekday("19971130\n"), None);
        assert_eq!(parse_first_weekday("19971130\n8\n"), None);
        assert_eq!(parse_first_weekday("garbage\n1\n"), None);
    }

    #[test]
    fn invalid_input() {
        assert!(month_grid(2026, 0, Weekday::Mon).is_none());
        assert!(month_grid(2026, 13, Weekday::Mon).is_none());
    }
}
