/// A GRT token amount stored as f64.
/// All public APIs accept/return Grt; wei conversion is internal.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub struct Grt(pub f64);

impl Grt {
    pub fn zero() -> Self {
        Self(0.0)
    }

    /// Parse a wei string (as returned by network subgraph) into GRT.
    pub fn from_wei(s: &str) -> Self {
        let v: u128 = s.trim().parse().unwrap_or(0);
        Self(v as f64 / 1e18)
    }

    /// Serialise to wei string (for setIndexingRule etc.).
    pub fn to_wei_str(self) -> String {
        format!("{}", (self.0 * 1e18) as u128)
    }

    /// Human-readable with thousands separators, no decimals for whole numbers.
    pub fn display(self) -> String {
        fmt_comma(self.0)
    }
}

impl std::fmt::Display for Grt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} GRT", fmt_comma(self.0))
    }
}

/// Format a float with comma thousands separators, no fractional part.
pub fn fmt_comma(v: f64) -> String {
    let i = v.round() as u64;
    let s = i.to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, c) in chars.iter().enumerate() {
        if idx > 0 && (chars.len() - idx) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// Format USD amount.
pub fn fmt_usd(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("${:.0}", v)
    } else {
        format!("${:.2}", v)
    }
}
