//! Detection of secrets (API keys, tokens, private keys, credentials in
//! URLs) in clipboard text, so they are not sent to the LLM by accident.
//!
//! This complements the password-manager markers in `clipboard_backend`:
//! it catches secrets copied by hand from a terminal, a `.env` file or a
//! web console, which carry no marker. Patterns are deliberately specific -
//! vendor prefixes, fixed lengths and character sets taken from each
//! vendor's real format - so normal prose and code rarely match. Generic
//! `password=...` style assignments are not matched (too many false
//! positives). URLs with a username and password do match, including
//! documentation placeholders like `postgres://user:pass@localhost`.
//!
//! Detection can be turned off with `--no-secret-check`/`-s` or the
//! `secret_check=false` config key. Only the kind of secret is ever
//! reported, never the matched text.

use regex::RegexSet;
use std::sync::OnceLock;

/// (kind, pattern) pairs. When several match, the first one listed is
/// reported, so more specific patterns come before broader ones.
///
/// `\b` is rewritten to an ASCII-only word boundary when compiling (see
/// [`compile`]); all patterns are anchored on ASCII text anyway.
const PATTERNS: &[(&str, &str)] = &[
    // Private keys
    (
        "private key",
        r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----",
    ),
    ("age secret key", r"AGE-SECRET-KEY-1[0-9A-Z]{58}"),
    // LLM providers
    (
        "Anthropic API key",
        r"\bsk-ant-(?:api|admin)[0-9]{2}-[A-Za-z0-9_-]{32,}",
    ),
    ("OpenRouter API key", r"\bsk-or-v1-[a-f0-9]{64}"),
    (
        "OpenAI API key",
        r"\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{40,}",
    ),
    ("OpenAI API key", r"\bsk-[A-Za-z0-9]{32,}"),
    ("Groq API key", r"\bgsk_[A-Za-z0-9]{48,}"),
    ("xAI API key", r"\bxai-[A-Za-z0-9]{40,}"),
    ("Hugging Face token", r"\bhf_[A-Za-z0-9]{34,}"),
    ("Replicate API token", r"\br8_[A-Za-z0-9]{30,}"),
    ("Perplexity API key", r"\bpplx-[A-Za-z0-9]{40,}"),
    // Cloud
    (
        "AWS access key ID",
        r"\b(?:AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b",
    ),
    (
        "AWS secret access key",
        r"(?i)aws_secret_access_key\s*[=:]\s*[A-Za-z0-9/+]{40}",
    ),
    ("Google API key", r"\bAIza[0-9A-Za-z_-]{35}"),
    ("Google OAuth client secret", r"\bGOCSPX-[A-Za-z0-9_-]{28}"),
    ("Google OAuth access token", r"\bya29\.[0-9A-Za-z_-]{30,}"),
    (
        "Azure access key",
        r"(?:AccountKey|SharedAccessKey)=[A-Za-z0-9+/]{40,}={0,2}",
    ),
    ("DigitalOcean token", r"\bdo[por]_v1_[a-f0-9]{64}"),
    ("Databricks token", r"\bdapi[a-f0-9]{32}"),
    ("HashiCorp Vault token", r"\bhv[sb]\.[A-Za-z0-9_-]{24,}"),
    (
        "Doppler token",
        r"\bdp\.(?:pt|st|sa|ct|scim|audit)\.[A-Za-z0-9]{40,}",
    ),
    // Code hosting and package registries
    ("GitHub token", r"\bgh[pousr]_[A-Za-z0-9]{36,}"),
    ("GitHub token", r"\bgithub_pat_[A-Za-z0-9_]{80,}"),
    (
        "GitLab token",
        r"\bgl(?:pat|dt|rt|ptt|cbt|imt|oas|ffct|soat|agent)-[A-Za-z0-9_-]{20,}",
    ),
    ("npm token", r"\bnpm_[A-Za-z0-9]{36}"),
    ("PyPI token", r"\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,}"),
    // SaaS
    ("Slack token", r"\bxox[abposr]-[0-9A-Za-z-]{10,}"),
    (
        "Slack app token",
        r"\bxapp-[0-9]-[A-Z0-9]+-[0-9]+-[a-z0-9]+",
    ),
    (
        "Slack webhook URL",
        r"https://hooks\.slack\.com/(?:services|workflows|triggers)/[A-Za-z0-9+/_-]{20,}",
    ),
    (
        "Stripe secret key",
        r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{24,}",
    ),
    ("Stripe webhook secret", r"\bwhsec_[A-Za-z0-9]{32,}"),
    (
        "SendGrid API key",
        r"\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}",
    ),
    ("Shopify token", r"\bshp(?:at|ca|pa|ss)_[a-fA-F0-9]{32}"),
    ("Atlassian API token", r"\bATATT3[A-Za-z0-9_=-]{150,}"),
    ("Linear API key", r"\blin_api_[A-Za-z0-9]{40}"),
    ("Notion token", r"\b(?:ntn|secret)_[A-Za-z0-9]{40,}"),
    ("Twilio API key", r"\bSK[0-9a-fA-F]{32}\b"),
    ("Mailgun API key", r"\bkey-[0-9a-f]{32}\b"),
    ("Mailchimp API key", r"\b[0-9a-f]{32}-us[0-9]{1,2}\b"),
    ("Postman API key", r"\bPMAK-[a-f0-9]{24}-[a-f0-9]{34}"),
    ("Grafana token", r"\bglc_[A-Za-z0-9+/]{32,}"),
    ("Grafana token", r"\bglsa_[A-Za-z0-9]{32}_[a-f0-9]{8}"),
    ("Sentry token", r"\bsntry[su]_[A-Za-z0-9+/=_]{40,}"),
    ("Square token", r"\bsq0(?:atp|csp)-[0-9A-Za-z_-]{22,}"),
    // Bots and webhooks
    ("Telegram bot token", r"\b[0-9]{8,10}:AA[A-Za-z0-9_-]{33}"),
    (
        "Discord bot token",
        r"\b[MNO][A-Za-z0-9_-]{23,25}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,38}\b",
    ),
    (
        "Discord webhook URL",
        r"https://(?:ptb\.|canary\.)?discord(?:app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9_-]{60,}",
    ),
    // Generic token formats
    (
        "JSON Web Token",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{10,}",
    ),
    (
        "URL with password",
        r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/?#@]*:[^\s/?#@]+@[^\s/?#@]+",
    ),
];

/// Compile [`PATTERNS`] into one set. `\b` is replaced by the ASCII-only
/// `(?-u:\b)`: a Unicode word boundary would make the regex engine fall
/// back to its slowest matcher on any non-ASCII clipboard text.
fn compile() -> RegexSet {
    let patterns = PATTERNS
        .iter()
        .map(|(_, pattern)| pattern.replace(r"\b", r"(?-u:\b)"));
    RegexSet::new(patterns).expect("built-in secret patterns are valid")
}

/// The compiled pattern set, built on first use.
fn pattern_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(compile)
}

/// Return the kind of the first secret found in `text` (e.g. "GitHub
/// token"), or `None`. The matched text itself is never returned.
pub fn find_secret(text: &str) -> Option<&'static str> {
    pattern_set()
        .matches(text)
        .iter()
        .next()
        .map(|index| PATTERNS[index].0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test secrets are assembled at runtime from split pieces, so this
    // public repository never contains a string that a secret scanner
    // (GitHub push protection, gitleaks, ...) would flag as a real key.

    /// `prefix` split as given, followed by `n` repetitions of `body`.
    fn fake(prefix: &[&str], body: &str, n: usize) -> String {
        format!("{}{}", prefix.concat(), body.repeat(n))
    }

    /// Assert `secret` is detected as `kind`, alone and inside prose.
    fn assert_kind(secret: &str, kind: &str) {
        assert_eq!(find_secret(secret), Some(kind), "alone: {secret}");
        let embedded = format!("Please fix my config:\n  token = \"{secret}\"\nthanks");
        assert_eq!(find_secret(&embedded), Some(kind), "embedded: {secret}");
    }

    #[test]
    fn all_patterns_compile() {
        assert_eq!(compile().len(), PATTERNS.len());
    }

    #[test]
    fn private_keys() {
        for k in ["RSA ", "EC ", "DSA ", "OPENSSH ", "ENCRYPTED ", ""] {
            let pem = format!(
                "-----BEGIN {k}{}-----\nMIIabc\n",
                ["PRIVATE", " KEY"].concat()
            );
            assert_kind(&pem, "private key");
        }
        let pgp = format!("-----BEGIN PGP {} BLOCK-----", ["PRIVATE", " KEY"].concat());
        assert_kind(&pgp, "private key");
        assert_kind(&fake(&["AGE-SECRET", "-KEY-1"], "Q", 58), "age secret key");
    }

    #[test]
    fn public_keys_and_certs_are_not_secrets() {
        assert_eq!(find_secret("-----BEGIN PUBLIC KEY-----"), None);
        assert_eq!(find_secret("-----BEGIN CERTIFICATE-----"), None);
        assert_eq!(
            find_secret("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA user@host"),
            None
        );
    }

    #[test]
    fn llm_provider_keys() {
        assert_kind(
            &fake(&["sk-", "ant-api03-"], "aB3_", 20),
            "Anthropic API key",
        );
        assert_kind(
            &fake(&["sk-", "ant-admin01-"], "aB3_", 20),
            "Anthropic API key",
        );
        assert_kind(&fake(&["sk-", "or-v1-"], "a1", 32), "OpenRouter API key");
        assert_kind(&fake(&["sk-", "proj-"], "aB3_", 20), "OpenAI API key");
        assert_kind(&fake(&["sk-", "svcacct-"], "aB3-", 20), "OpenAI API key");
        assert_kind(&fake(&["s", "k-"], "aB3", 16), "OpenAI API key");
        assert_kind(&fake(&["gs", "k_"], "aB3", 18), "Groq API key");
        assert_kind(&fake(&["xa", "i-"], "aB3", 27), "xAI API key");
        assert_kind(&fake(&["h", "f_"], "aB", 17), "Hugging Face token");
        assert_kind(&fake(&["r", "8_"], "aB3", 13), "Replicate API token");
        assert_kind(&fake(&["pp", "lx-"], "aB3", 16), "Perplexity API key");
    }

    #[test]
    fn cloud_keys() {
        for p in ["AK", "AS", "AB", "AC"] {
            let id = format!(
                "{p}{}{}",
                if p == "AC" { "CA" } else { "IA" },
                "ABCDEFGH23456789"
            );
            assert_kind(&id, "AWS access key ID");
        }
        let aws = format!("aws_secret_access_key = {}", "aB3/".repeat(10));
        assert_kind(&aws, "AWS secret access key");
        assert_kind(&fake(&["AI", "za"], "aB3_-", 7), "Google API key");
        assert_kind(
            &fake(&["GOC", "SPX-"], "aB3_", 7),
            "Google OAuth client secret",
        );
        assert_kind(
            &fake(&["ya", "29."], "aB3_", 10),
            "Google OAuth access token",
        );
        let azure = format!("{}={}==", ["Account", "Key"].concat(), "aB3+".repeat(22));
        assert_kind(&azure, "Azure access key");
        assert_kind(&fake(&["do", "p_v1_"], "a1", 32), "DigitalOcean token");
        assert_kind(&fake(&["da", "pi"], "a1", 16), "Databricks token");
        assert_kind(&fake(&["hv", "s."], "aB3_", 8), "HashiCorp Vault token");
        assert_kind(&fake(&["dp", ".pt."], "aB3", 15), "Doppler token");
    }

    #[test]
    fn code_hosting_keys() {
        for p in ["p", "o", "u", "s", "r"] {
            assert_kind(&fake(&["gh", p, "_"], "aB3", 12), "GitHub token");
        }
        assert_kind(&fake(&["github", "_pat_"], "aB3_", 21), "GitHub token");
        assert_kind(&fake(&["gl", "pat-"], "aB3_", 5), "GitLab token");
        assert_kind(&fake(&["gl", "rt-"], "aB3_", 5), "GitLab token");
        assert_kind(&fake(&["np", "m_"], "aB3", 12), "npm token");
        assert_kind(
            &fake(&["pypi-", "AgEIcHlwaS5vcmc"], "aB3_", 13),
            "PyPI token",
        );
    }

    #[test]
    fn saas_keys() {
        assert_kind(&fake(&["xo", "xb-"], "123-aB", 4), "Slack token");
        assert_kind(
            &format!(
                "{}-1-A1B2C3-12345-{}",
                ["xa", "pp"].concat(),
                "a1".repeat(10)
            ),
            "Slack app token",
        );
        assert_kind(
            &fake(&["https://hooks.", "slack.com/services/"], "T0/B1", 6),
            "Slack webhook URL",
        );
        assert_kind(&fake(&["sk", "_live_"], "aB3", 9), "Stripe secret key");
        assert_kind(&fake(&["rk", "_test_"], "aB3", 9), "Stripe secret key");
        assert_kind(&fake(&["wh", "sec_"], "aB3", 11), "Stripe webhook secret");
        let sg = format!(
            "{}{}.{}",
            ["S", "G."].concat(),
            "aB3_-".repeat(4) + "aB",
            "aB3_-".repeat(8) + "aB3"
        );
        assert_kind(&sg, "SendGrid API key");
        assert_kind(&fake(&["shp", "at_"], "a1", 16), "Shopify token");
        assert_kind(&fake(&["ATA", "TT3"], "aB3_", 40), "Atlassian API token");
        assert_kind(&fake(&["lin", "_api_"], "aB3D", 10), "Linear API key");
        assert_kind(&fake(&["nt", "n_"], "aB3", 15), "Notion token");
        assert_kind(&fake(&["S", "K"], "a1", 16), "Twilio API key");
        assert_kind(&fake(&["ke", "y-"], "a1", 16), "Mailgun API key");
        assert_kind(
            &format!("{}-us{}", "a1".repeat(16), 14),
            "Mailchimp API key",
        );
        let pmak = format!(
            "{}{}-{}",
            ["PM", "AK-"].concat(),
            "a1".repeat(12),
            "a1".repeat(17)
        );
        assert_kind(&pmak, "Postman API key");
        assert_kind(&fake(&["gl", "c_"], "aB3+", 9), "Grafana token");
        assert_kind(
            &format!(
                "{}{}_{}",
                ["gl", "sa_"].concat(),
                "aB3D".repeat(8),
                "a1".repeat(4)
            ),
            "Grafana token",
        );
        assert_kind(&fake(&["snt", "rys_"], "aB3=", 11), "Sentry token");
        assert_kind(&fake(&["sq0", "atp-"], "aB3_", 6), "Square token");
    }

    #[test]
    fn bot_tokens_and_webhooks() {
        assert_kind(
            &format!("123456789:{}{}", "A", "A".to_string() + &"aB3".repeat(11)),
            "Telegram bot token",
        );
        let discord = format!("M{}.{}.{}", "aB3_-".repeat(5), "aB3_-a", "aB3_-".repeat(6));
        assert_kind(&discord, "Discord bot token");
        assert_kind(
            &fake(
                &["https://discord.com/api/", "webhooks/1234567890/"],
                "aB3_-",
                13,
            ),
            "Discord webhook URL",
        );
    }

    #[test]
    fn jwt() {
        let jwt = format!(
            "{}{}.{}{}.{}",
            "ey",
            "JhbGciOiJIUzI1NiJ9",
            "ey",
            "JzdWIiOiIxMjMifQ",
            "aB3_-".repeat(4)
        );
        assert_kind(&jwt, "JSON Web Token");
    }

    #[test]
    fn urls_with_password() {
        assert_kind(
            "postgres://admin:hunter2@db.internal:5432/app",
            "URL with password",
        );
        assert_kind("redis://:s3cret@127.0.0.1:6379", "URL with password");
        assert_kind("https://user:pa55@example.com/path", "URL with password");
    }

    #[test]
    fn urls_without_password_are_fine() {
        for text in [
            "https://example.com/path?q=1",
            "ssh://git@github.com/owner/repo.git",
            "http://[::1]:11434/v1/chat/completions",
            "https://user@example.com/",
            "mailto:someone@example.com",
            "Meet at 10:30@office",
        ] {
            assert_eq!(find_secret(text), None, "{text}");
        }
    }

    #[test]
    fn normal_text_is_not_a_secret() {
        for text in [
            "Arr, ye scurvy dog! Check the sk-learn docs and scikit-learn.",
            "AKIA is short, AKIA1234 too, and AKIASHORT.",
            "The ghp_ prefix is used by GitHub; so is github_pat_.",
            "eyJ alone is not a token. eyJhbGci.eyJ.",
            "key-value store, key-12345, secret_sauce, hf_transfer",
            "Call me at 123456789: AA batteries needed.",
            "fn main() { let sk = \"sk-\"; println!(\"{}\", sk); }",
            "-----BEGIN PGP SIGNED MESSAGE-----",
            "Ünïcödé text, 日本語, emoji \u{2764}\u{FE0F}",
            "",
        ] {
            assert_eq!(find_secret(text), None, "{text}");
        }
    }

    #[test]
    fn short_or_wrong_charset_near_misses() {
        assert_eq!(find_secret(&fake(&["gh", "p_"], "a", 20)), None);
        assert_eq!(find_secret(&fake(&["s", "k-"], "a-b", 12)), None);
        assert_eq!(find_secret(&fake(&["do", "p_v1_"], "Z", 64)), None);
        assert_eq!(find_secret(&fake(&["AI", "za"], "a", 20)), None);
        // Prefix glued to a preceding word is not a token start.
        assert_eq!(find_secret(&fake(&["xgh", "p_"], "aB3", 12)), None);
    }

    #[test]
    fn real_documents_have_no_false_positives() {
        // Long real-world prose and config text from this repository.
        for (name, text) in [
            ("LICENSE", include_str!("../LICENSE")),
            ("README.md", include_str!("../README.md")),
            ("config.ini.example", include_str!("../config.ini.example")),
        ] {
            assert_eq!(find_secret(text), None, "{name}");
        }
    }

    #[test]
    fn first_listed_kind_wins() {
        // An Anthropic key also looks like a generic token; the specific
        // kind is reported.
        let key = fake(&["sk-", "ant-api03-"], "aB3_", 20);
        assert_eq!(find_secret(&key), Some("Anthropic API key"));
    }
}
