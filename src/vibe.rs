/// MCPVOT — the git identity / vibe behind this tool.
/// Rendered by `hermes-update-guard vibe` and as a compact header on
/// preflight/update so the operator sees whose guard this is.
pub const VIBE: &str = r#"___  ________ ______ _   _  _____ _____ 
|  \/  /  __ \| ___ \ | | ||  _  |_   _|
| .  . | /  \/| |_/ / | | || | | | | |  
| |\/| | |    |  __/| | | || | | | | |  
| |  | | \__/\| |   \ \_/ /\ \_/ / | |  
\_|  |_/\____/\_|    \___/  \___/  \_/  "#;

pub const TAGLINE: &str = "MCPVOT · hermes-update-guard — guard the update, keep the edge.";

pub fn print_full() {
    println!("{}", VIBE);
    println!("{}", TAGLINE);
}

pub fn print_compact(version: &str) {
    println!("════════════════════════════════════════════");
    println!("MCPVOT · hermes-update-guard v{}", version);
    println!("════════════════════════════════════════════");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vibe_is_ascii_only() {
        assert!(
            VIBE.is_ascii(),
            "vibe banner must stay pure ASCII (terminal-safe everywhere)"
        );
        // TAGLINE intentionally uses the brand middle-dot (·) and dash (—); only the
        // figure itself must be portable ASCII.
    }

    #[test]
    fn vibe_has_no_trailing_blank_line() {
        assert!(
            !VIBE.ends_with('\n'),
            "banner must not end with a stray blank line"
        );
    }
}
