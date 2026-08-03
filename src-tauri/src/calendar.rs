use chrono::NaiveDate;

const DAY_FORMAT: &str = "%Y-%m-%d";

pub fn parse(day: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day, DAY_FORMAT).ok()
}

pub fn offset(day: &str, days: i64) -> String {
    parse(day)
        .map(|date| {
            (date + chrono::Duration::days(days))
                .format(DAY_FORMAT)
                .to_string()
        })
        .unwrap_or_else(|| day.to_owned())
}

pub fn format(day: &str, format: &str) -> String {
    parse(day)
        .map(|date| date.format(format).to_string())
        .unwrap_or_else(|| day.to_owned())
}
