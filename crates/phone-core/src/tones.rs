//! The call-progress tones a caller expects to hear, country by country.
//!
//! Ringback ("it's ringing"), busy, and congestion ("the network can't connect
//! you") differ by country, and people know their own by ear: an American hearing
//! Australia's double ring thinks something is wrong. The network usually sends
//! ringback itself, but busy and congestion are played by the phone once the call
//! has been released, and some networks send silence while ringing out, so the
//! phone needs all three. Values are from ITU-T E.180 (Supplement 2, "Various
//! tones used in national networks") and the national specifications it cites.

/// Which tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Progress {
    Ringback,
    Busy,
    Congestion,
}

impl Progress {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ringback => "ringback",
            Self::Busy => "busy",
            Self::Congestion => "congestion",
        }
    }
}

/// A tone: frequencies sounded together, and a cadence of alternating on and off
/// times in milliseconds (on first). An empty cadence is a continuous tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cadence {
    pub freqs: &'static [f32],
    pub steps: &'static [u32],
}

impl Cadence {
    /// One cycle's length in milliseconds (a continuous tone counts as one second).
    pub fn cycle_ms(&self) -> u32 {
        if self.steps.is_empty() {
            1000
        } else {
            self.steps.iter().sum()
        }
    }
}

const fn c(freqs: &'static [f32], steps: &'static [u32]) -> Cadence {
    Cadence { freqs, steps }
}

/// The tone for `region` (ISO 3166 alpha-2), falling back to the CEPT tones most
/// of Europe and much of the world use.
pub fn cadence(region: &str, p: Progress) -> Cadence {
    use Progress::*;
    // Amplitude modulation (India's 400 Hz × 25 Hz, Japan's 400 Hz × 16 Hz) is
    // written as the carrier and its two sidebands, which is the same sound.
    const AU_RING: Cadence = c(&[400.0, 425.0, 450.0], &[400, 200, 400, 2000]);
    const UK_RING: Cadence = c(&[400.0, 450.0], &[400, 200, 400, 2000]);
    const NA_RING: Cadence = c(&[440.0, 480.0], &[2000, 4000]);
    const NA_BUSY: Cadence = c(&[480.0, 620.0], &[500, 500]);
    const NA_REORDER: Cadence = c(&[480.0, 620.0], &[250, 250]);
    const CEPT_RING: Cadence = c(&[425.0], &[1000, 4000]);
    const CEPT_BUSY: Cadence = c(&[425.0], &[500, 500]);
    const CEPT_CONGESTION: Cadence = c(&[425.0], &[250, 250]);
    match (region.to_ascii_uppercase().as_str(), p) {
        // Australia (Communications Alliance S002).
        ("AU", Ringback) => AU_RING,
        ("AU", Busy) => c(&[425.0], &[375, 375]),
        ("AU", Congestion) => c(&[425.0], &[375, 375, 375, 375]),
        ("NZ", Ringback) => UK_RING,
        ("NZ", Busy) => c(&[400.0], &[500, 500]),
        ("NZ", Congestion) => c(&[400.0], &[250, 250]),
        ("GB" | "IE", Ringback) => UK_RING,
        ("GB" | "IE", Busy) => c(&[400.0], &[375, 375]),
        ("GB" | "IE", Congestion) => c(&[400.0], &[400, 350, 225, 525]),
        // North America (the NANP countries share them).
        ("US" | "CA" | "PR" | "JM" | "BS" | "BB" | "TT", Ringback) => NA_RING,
        ("US" | "CA" | "PR" | "JM" | "BS" | "BB" | "TT", Busy) => NA_BUSY,
        ("US" | "CA" | "PR" | "JM" | "BS" | "BB" | "TT", Congestion) => NA_REORDER,
        ("FR", Ringback) => c(&[440.0], &[1500, 3500]),
        ("FR", Busy) => c(&[440.0], &[500, 500]),
        ("FR", Congestion) => c(&[440.0], &[250, 250]),
        ("DE", Busy) => c(&[425.0], &[480, 480]),
        ("DE", Congestion) => c(&[425.0], &[240, 240]),
        ("IT", Ringback) => c(&[425.0], &[1000, 4000]),
        ("IT", Busy) => c(&[425.0], &[500, 500]),
        ("IT", Congestion) => c(&[425.0], &[200, 200]),
        ("JP", Ringback) => c(&[384.0, 400.0, 416.0], &[1000, 2000]),
        ("JP", Busy | Congestion) => c(&[400.0], &[500, 500]),
        ("IN", Ringback) => c(&[375.0, 400.0, 425.0], &[400, 200, 400, 2000]),
        ("IN", Busy) => c(&[400.0], &[750, 750]),
        ("IN", Congestion) => c(&[400.0], &[250, 250]),
        ("CN", Ringback) => c(&[450.0], &[1000, 4000]),
        ("CN", Busy) => c(&[450.0], &[350, 350]),
        ("CN", Congestion) => c(&[450.0], &[700, 700]),
        ("HK", Ringback) => c(&[440.0, 480.0], &[400, 200, 400, 3000]),
        ("HK", Busy) => c(&[480.0, 620.0], &[500, 500]),
        ("HK", Congestion) => c(&[480.0, 620.0], &[250, 250]),
        ("SG" | "MY", Ringback) => c(&[425.0], &[400, 200, 400, 2000]),
        ("SG" | "MY", Busy) => c(&[425.0], &[750, 750]),
        ("SG" | "MY", Congestion) => c(&[425.0], &[250, 250]),
        ("KR", Ringback) => c(&[440.0, 480.0], &[1000, 2000]),
        ("KR", Busy) => c(&[480.0, 620.0], &[500, 500]),
        ("KR", Congestion) => c(&[480.0, 620.0], &[300, 200]),
        ("BR", Ringback) => c(&[425.0], &[1000, 4000]),
        ("BR", Busy) => c(&[425.0], &[250, 250]),
        ("BR", Congestion) => c(&[425.0], &[750, 250, 250, 250]),
        ("ZA", Ringback) => c(&[400.0, 433.0], &[400, 200, 400, 2000]),
        ("ZA", Busy) => c(&[400.0], &[500, 500]),
        ("ZA", Congestion) => c(&[400.0], &[250, 250]),
        // CEPT: the rest of Europe, and the default.
        (_, Ringback) => CEPT_RING,
        (_, Busy) => CEPT_BUSY,
        (_, Congestion) => CEPT_CONGESTION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries_sound_like_themselves() {
        assert_eq!(cadence("AU", Progress::Ringback).steps, &[400, 200, 400, 2000], "Australia's double ring");
        assert_eq!(cadence("us", Progress::Ringback).freqs, &[440.0, 480.0], "case does not matter");
        assert_eq!(cadence("US", Progress::Busy).steps, &[500, 500]);
        assert_eq!(cadence("FR", Progress::Ringback).freqs, &[440.0]);
        assert_eq!(cadence("NL", Progress::Ringback), cadence("XX", Progress::Ringback), "CEPT is the default");
    }

    #[test]
    fn cadences_are_well_formed() {
        for r in ["AU", "NZ", "GB", "US", "FR", "DE", "IT", "JP", "IN", "CN", "HK", "SG", "KR", "BR", "ZA", "XX"] {
            for p in [Progress::Ringback, Progress::Busy, Progress::Congestion] {
                let t = cadence(r, p);
                assert!(!t.freqs.is_empty(), "{r} {p:?}");
                assert!(t.steps.len().is_multiple_of(2), "{r} {p:?}: on/off pairs");
                assert!(t.freqs.iter().all(|f| (300.0..700.0).contains(f)), "{r} {p:?}: tones are low and plain");
                assert!((400..=6000).contains(&t.cycle_ms()), "{r} {p:?}");
            }
        }
    }
}
