pub fn truncate(value: &str, max_chars: usize) -> String {
    value.char_indices().nth(max_chars).map_or_else(
        || value.to_owned(),
        |(end, _)| format!("{}…", &value[..end]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("aé日z", 3), "aé日…");
    }

    #[test]
    fn leaves_short_values_unchanged() {
        assert_eq!(truncate("aé日", 3), "aé日");
    }
}
