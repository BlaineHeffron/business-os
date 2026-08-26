//! Shared date/datetime normalization for typed AI and operator inputs.
//!
//! Persisted and wire values stay strict: civil dates are `YYYY-MM-DD`, and
//! datetimes are RFC3339 with an explicit offset or `Z`. Human/model date
//! variants are accepted only by callers that pass an explicit context date.

use bos_contracts::email_triage::InboundMessageRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateInputContext {
    pub anchor_date: String,
    pub anchor_datetime_utc: String,
    pub timezone_hint: String,
}

impl DateInputContext {
    pub fn from_epoch_ms(epoch_ms: u64, timezone_hint: impl Into<String>) -> Self {
        Self {
            anchor_date: epoch_ms_to_utc_date(epoch_ms),
            anchor_datetime_utc: epoch_ms_to_rfc3339_utc(epoch_ms),
            timezone_hint: timezone_hint.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateInputError {
    ContextRequired,
    Invalid,
}

pub fn context_from_email(message: &InboundMessageRecord) -> DateInputContext {
    let epoch_ms = message
        .internal_date_ms
        .unwrap_or(message.ingested_at_ms as i64)
        .max(0) as u64;
    DateInputContext::from_epoch_ms(epoch_ms, "UTC")
}

pub fn context_from_now_ms(now_ms: u64) -> DateInputContext {
    DateInputContext::from_epoch_ms(now_ms, "UTC")
}

pub fn email_prompt_datetime_context(message: &InboundMessageRecord) -> String {
    if message.internal_date_ms.is_some() {
        return email_source_date_block(message.internal_date_ms);
    }
    let context = context_from_email(message);
    format!(
        "Date (epoch ms): (unknown)\nEmail date (UTC): {}\nEmail datetime (UTC): {}\nDate output format: YYYY-MM-DD. Datetime output format: RFC3339 with explicit offset or Z.",
        context.anchor_date, context.anchor_datetime_utc
    )
}

pub fn normalize_civil_date(
    raw: &str,
    context: Option<&DateInputContext>,
) -> Result<String, DateInputError> {
    let normalized = normalize_spaces(raw);
    if normalized.is_empty() {
        return Err(DateInputError::Invalid);
    }
    if let Some(date) = strict_civil_date(&normalized) {
        return Ok(date);
    }
    if let Some(date) = rfc3339_date_part(&normalized) {
        return Ok(date);
    }
    let context = context.ok_or(DateInputError::ContextRequired)?;
    parse_contextual_date(&normalized, context)
}

pub fn normalize_rfc3339_datetime(raw: &str) -> Result<String, DateInputError> {
    let trimmed = raw.trim();
    if is_rfc3339_with_offset(trimmed) {
        let mut canonical = trimmed.to_string();
        if canonical.as_bytes().get(10) == Some(&b't') {
            canonical.replace_range(10..11, "T");
        }
        if canonical.ends_with('z') {
            canonical.pop();
            canonical.push('Z');
        }
        Ok(canonical)
    } else {
        Err(DateInputError::Invalid)
    }
}

pub fn is_civil_date(raw: &str) -> bool {
    strict_civil_date(raw).is_some()
}

fn email_source_date_block(internal_date_ms: Option<i64>) -> String {
    let Some(epoch_ms) = internal_date_ms else {
        return "Date (epoch ms): (unknown)\nEmail date (UTC): (unknown)\nEmail datetime (UTC): (unknown)\nDate output format: YYYY-MM-DD. Datetime output format: RFC3339 with explicit offset or Z.".to_string();
    };
    let context = DateInputContext::from_epoch_ms(epoch_ms.max(0) as u64, "UTC");
    format!(
        "Date (epoch ms): {epoch_ms}\nEmail date (UTC): {}\nEmail datetime (UTC): {}\nDate output format: YYYY-MM-DD. Datetime output format: RFC3339 with explicit offset or Z.",
        context.anchor_date, context.anchor_datetime_utc
    )
}

pub fn is_rfc3339_with_offset(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let date_ok = digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && bytes[4] == b'-'
        && digit(5)
        && digit(6)
        && bytes[7] == b'-'
        && digit(8)
        && digit(9);
    if !date_ok || strict_civil_date(&raw[..10]).is_none() {
        return false;
    }
    let time_ok = (bytes[10] == b'T' || bytes[10] == b't')
        && digit(11)
        && digit(12)
        && bytes[13] == b':'
        && digit(14)
        && digit(15)
        && bytes[16] == b':'
        && digit(17)
        && digit(18);
    if !time_ok {
        return false;
    }
    let Ok(hour) = raw[11..13].parse::<u32>() else {
        return false;
    };
    let Ok(minute) = raw[14..16].parse::<u32>() else {
        return false;
    };
    let Ok(second) = raw[17..19].parse::<u32>() else {
        return false;
    };
    if hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let mut rest = &raw[19..];
    if let Some(stripped) = rest.strip_prefix('.') {
        let frac_len = stripped.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len == 0 {
            return false;
        }
        rest = &stripped[frac_len..];
    }
    if rest == "Z" || rest == "z" {
        return true;
    }
    let rest_bytes = rest.as_bytes();
    if rest_bytes.len() != 6
        || !matches!(rest_bytes[0], b'+' | b'-')
        || !rest_bytes[1].is_ascii_digit()
        || !rest_bytes[2].is_ascii_digit()
        || rest_bytes[3] != b':'
        || !rest_bytes[4].is_ascii_digit()
        || !rest_bytes[5].is_ascii_digit()
    {
        return false;
    }
    let Ok(offset_hour) = rest[1..3].parse::<u32>() else {
        return false;
    };
    let Ok(offset_minute) = rest[4..6].parse::<u32>() else {
        return false;
    };
    offset_hour <= 23 && offset_minute <= 59
}

pub fn epoch_ms_to_rfc3339_utc(epoch_ms: u64) -> String {
    let secs = (epoch_ms / 1_000) as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub fn epoch_ms_to_utc_date(epoch_ms: u64) -> String {
    epoch_ms_to_rfc3339_utc(epoch_ms).chars().take(10).collect()
}

pub fn civil_day_number(date: &str) -> Option<i64> {
    let canonical = strict_civil_date(date)?;
    let (year, month, day) = parse_ymd(&canonical)?;
    Some(days_from_civil(year, month, day))
}

fn parse_contextual_date(
    normalized: &str,
    context: &DateInputContext,
) -> Result<String, DateInputError> {
    let anchor = parse_ymd(&context.anchor_date).ok_or(DateInputError::Invalid)?;
    let anchor_year = anchor.0;
    let lower = normalized.to_ascii_lowercase();
    let compact = lower.replace(',', " ");
    let tokens = compact
        .split_whitespace()
        .filter(|token| *token != "the" && *token != "on")
        .collect::<Vec<_>>();

    if let Some(date) = parse_relative_date(&tokens, &context.anchor_date) {
        return Ok(date);
    }
    if let Some(date) = parse_numeric_date(&tokens, &context.anchor_date) {
        return Ok(date);
    }
    if let Some(date) = parse_month_name_date(&tokens, anchor_year, &context.anchor_date) {
        return Ok(date);
    }
    if let Some(date) = parse_day_or_weekday_date(&tokens, &context.anchor_date) {
        return Ok(date);
    }
    Err(DateInputError::Invalid)
}

fn parse_relative_date(tokens: &[&str], anchor_date: &str) -> Option<String> {
    if tokens.len() != 1 {
        return None;
    }
    match tokens[0] {
        "today" => Some(anchor_date.to_string()),
        "tomorrow" => {
            let day = civil_day_number(anchor_date)?;
            let (year, month, date_day) = civil_from_days(day + 1);
            canonical_date(year, month, date_day)
        }
        _ => None,
    }
}

fn parse_numeric_date(tokens: &[&str], anchor_date: &str) -> Option<String> {
    if tokens.len() != 1 {
        return None;
    }
    let separator = if tokens[0].contains('/') {
        '/'
    } else if tokens[0].contains('.') {
        '.'
    } else {
        return None;
    };
    let parts = tokens[0].split(separator).collect::<Vec<_>>();
    if !(parts.len() == 2 || parts.len() == 3) {
        return None;
    }
    let month = parts[0].parse::<u32>().ok()?;
    let day = parts[1].parse::<u32>().ok()?;
    if month > 12 {
        return None;
    }
    let anchor = parse_ymd(anchor_date)?;
    let year = match parts.get(2) {
        Some(raw) if raw.len() == 4 => raw.parse::<i64>().ok()?,
        Some(raw) if raw.len() == 2 => 2_000 + raw.parse::<i64>().ok()?,
        Some(_) => return None,
        None => return nearest_date_on_or_after(anchor_date, month, day),
    };
    let date = canonical_date(year, month, day)?;
    if parts.len() == 3 || date.as_str() >= anchor_date {
        Some(date)
    } else {
        canonical_date(anchor.0 + 1, month, day)
    }
}

fn parse_month_name_date(tokens: &[&str], anchor_year: i64, anchor_date: &str) -> Option<String> {
    let mut index = 0usize;
    let weekday = weekday_index(tokens.first().copied().unwrap_or(""));
    if weekday.is_some() {
        index += 1;
    }
    if !(tokens.len() == index + 2 || tokens.len() == index + 3) {
        return None;
    }
    let month = month_index(tokens.get(index).copied()?)?;
    let day = parse_day_token(tokens.get(index + 1).copied()?)?;
    let date = if tokens.len() == index + 3 {
        let year_token = tokens.get(index + 2)?;
        if year_token.len() != 4 {
            return None;
        }
        let year = year_token.parse::<i64>().ok()?;
        canonical_date(year, month, day)?
    } else {
        nearest_date_on_or_after(anchor_date, month, day)
            .or_else(|| canonical_date(anchor_year, month, day))?
    };
    if let Some(expected_weekday) = weekday {
        let actual = weekday_for_date(&date)?;
        if actual != expected_weekday {
            return None;
        }
    }
    Some(date)
}

fn parse_day_or_weekday_date(tokens: &[&str], anchor_date: &str) -> Option<String> {
    let anchor_day = civil_day_number(anchor_date)?;
    if tokens.len() == 1 {
        if let Some(weekday) = weekday_index(tokens[0]) {
            for offset in 1..=7 {
                let candidate_day = anchor_day + offset;
                if weekday_for_days(candidate_day) == weekday {
                    let (year, month, day) = civil_from_days(candidate_day);
                    return canonical_date(year, month, day);
                }
            }
            return None;
        }
        let day = parse_day_token(tokens[0])?;
        return nearest_day_of_month_on_or_after(anchor_date, day, None);
    }
    if tokens.len() != 2 {
        return None;
    }
    let weekday = weekday_index(tokens[0])?;
    let day = parse_day_token(tokens[1])?;
    nearest_day_of_month_on_or_after(anchor_date, day, Some(weekday))
}

fn nearest_date_on_or_after(anchor_date: &str, month: u32, day: u32) -> Option<String> {
    let (anchor_year, _, _) = parse_ymd(anchor_date)?;
    let current = canonical_date(anchor_year, month, day);
    match current {
        Some(date) if date.as_str() >= anchor_date => Some(date),
        _ => canonical_date(anchor_year + 1, month, day),
    }
}

fn nearest_day_of_month_on_or_after(
    anchor_date: &str,
    day: u32,
    weekday: Option<u32>,
) -> Option<String> {
    let (anchor_year, anchor_month, _) = parse_ymd(anchor_date)?;
    let anchor_month_index = anchor_year * 12 + i64::from(anchor_month) - 1;
    for offset in 0..=12 {
        let month_index = anchor_month_index + offset;
        let year = month_index.div_euclid(12);
        let month = month_index.rem_euclid(12) as u32 + 1;
        let Some(date) = canonical_date(year, month, day) else {
            continue;
        };
        if date.as_str() < anchor_date {
            continue;
        }
        if weekday
            .map(|expected| weekday_for_date(&date) == Some(expected))
            .unwrap_or(true)
        {
            return Some(date);
        }
        return None;
    }
    None
}

fn rfc3339_date_part(raw: &str) -> Option<String> {
    (raw.len() >= 20 && is_rfc3339_with_offset(raw)).then(|| raw[..10].to_string())
}

fn strict_civil_date(raw: &str) -> Option<String> {
    if raw.len() != 10 {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
    {
        return None;
    }
    let (year, month, day) = parse_ymd(raw)?;
    canonical_date(year, month, day)
}

fn parse_ymd(raw: &str) -> Option<(i64, u32, u32)> {
    if raw.len() != 10 {
        return None;
    }
    Some((
        raw[0..4].parse().ok()?,
        raw[5..7].parse().ok()?,
        raw[8..10].parse().ok()?,
    ))
}

fn canonical_date(year: i64, month: u32, day: u32) -> Option<String> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn parse_day_token(raw: &str) -> Option<u32> {
    let day = raw
        .trim_end_matches("st")
        .trim_end_matches("nd")
        .trim_end_matches("rd")
        .trim_end_matches("th")
        .parse::<u32>()
        .ok()?;
    (1..=31).contains(&day).then_some(day)
}

fn month_index(raw: &str) -> Option<u32> {
    match raw.trim_end_matches('.') {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

fn weekday_index(raw: &str) -> Option<u32> {
    match raw.trim_end_matches('.') {
        "mon" | "monday" => Some(0),
        "tue" | "tues" | "tuesday" => Some(1),
        "wed" | "wednesday" => Some(2),
        "thu" | "thur" | "thurs" | "thursday" => Some(3),
        "fri" | "friday" => Some(4),
        "sat" | "saturday" => Some(5),
        "sun" | "sunday" => Some(6),
        _ => None,
    }
}

fn weekday_for_date(date: &str) -> Option<u32> {
    Some(weekday_for_days(civil_day_number(date)?))
}

fn weekday_for_days(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

fn normalize_spaces(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = if shifted_year >= 0 {
        shifted_year
    } else {
        shifted_year - 399
    } / 400;
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DateInputContext {
        DateInputContext::from_epoch_ms(1_781_000_000_000, "UTC")
    }

    #[test]
    fn normalizes_contextual_model_dates() {
        let ctx = ctx();
        assert_eq!(
            normalize_civil_date("July 7", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("Jan 2", Some(&ctx)).unwrap(),
            "2027-01-02"
        );
        assert_eq!(
            normalize_civil_date("Tuesday July 7", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("Tuesday the 7th", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert!(normalize_civil_date("Monday the 7th", Some(&ctx)).is_err());
        assert!(normalize_civil_date("Wednesday July 7", Some(&ctx)).is_err());
        assert_eq!(
            normalize_civil_date("7/7/2026", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("7/7/26", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("7/7", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("the 7th", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("Wednesday", Some(&ctx)).unwrap(),
            "2026-06-10"
        );
        assert_eq!(
            normalize_civil_date("today", Some(&ctx)).unwrap(),
            "2026-06-09"
        );
        assert_eq!(
            normalize_civil_date("tomorrow", Some(&ctx)).unwrap(),
            "2026-06-10"
        );
        assert_eq!(
            normalize_civil_date("2026-07-07T18:30:00Z", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
    }

    #[test]
    fn omitted_dates_equal_to_reference_do_not_roll_forward() {
        let ctx = DateInputContext {
            anchor_date: "2026-07-07".to_string(),
            anchor_datetime_utc: "2026-07-07T12:00:00Z".to_string(),
            timezone_hint: "UTC".to_string(),
        };
        assert_eq!(
            normalize_civil_date("July 7", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("the 7th", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
        assert_eq!(
            normalize_civil_date("Tuesday the 7th", Some(&ctx)).unwrap(),
            "2026-07-07"
        );
    }

    #[test]
    fn two_digit_year_is_explicit_and_does_not_roll_forward() {
        let ctx = DateInputContext {
            anchor_date: "2026-12-20".to_string(),
            anchor_datetime_utc: "2026-12-20T12:00:00Z".to_string(),
            timezone_hint: "UTC".to_string(),
        };
        assert_eq!(
            normalize_civil_date("1/5/26", Some(&ctx)).unwrap(),
            "2026-01-05"
        );
        assert_eq!(
            normalize_civil_date("1/5", Some(&ctx)).unwrap(),
            "2027-01-05"
        );
    }

    #[test]
    fn rejects_ambiguous_or_bad_dates() {
        assert_eq!(
            normalize_civil_date("July 7", None).unwrap_err(),
            DateInputError::ContextRequired
        );
        assert!(normalize_civil_date("July 7 at 5pm", Some(&ctx())).is_err());
        assert!(normalize_civil_date("Tuesday July 7 2026 extra", Some(&ctx())).is_err());
        assert!(normalize_civil_date("Jul 7 26", Some(&ctx())).is_err());
        assert!(normalize_civil_date("Monday July 7", Some(&ctx())).is_err());
        assert!(normalize_civil_date("2026-02-29", Some(&ctx())).is_err());
        assert!(normalize_civil_date("next Tuesday", Some(&ctx())).is_err());
        assert!(normalize_civil_date("13/7/2026", Some(&ctx())).is_err());
    }

    #[test]
    fn civil_day_number_requires_valid_civil_date() {
        assert!(civil_day_number("2026-06-09").is_some());
        assert!(civil_day_number("2026-13-01").is_none());
        assert!(civil_day_number("2026-02-31").is_none());
        assert!(civil_day_number("2026-2-3").is_none());
    }

    #[test]
    fn validates_rfc3339_with_offset_and_ranges() {
        for ok in [
            "2026-06-12T16:00:00-04:00",
            "2026-06-12T20:00:00Z",
            "2026-06-12t20:00:00.123z",
        ] {
            assert!(is_rfc3339_with_offset(ok), "{ok}");
        }
        assert_eq!(
            normalize_rfc3339_datetime("2026-06-12t20:00:00.123z").unwrap(),
            "2026-06-12T20:00:00.123Z"
        );
        for bad in [
            "2026-06-12T16:00:00",
            "2026-06-12 16:00:00Z",
            "2026-13-12T16:00:00Z",
            "2026-06-12T26:00:00Z",
            "2026-06-12T16:00:00-04",
        ] {
            assert!(!is_rfc3339_with_offset(bad), "{bad}");
        }
    }
}
