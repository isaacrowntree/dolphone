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
    /// The level of each burst in turn, as a factor of full level, repeating; empty
    /// is every burst at full level. Australia's congestion tone is its busy tone
    /// with every second burst quieter, so this is all that tells the two apart.
    pub levels: &'static [f32],
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

    /// The level of the `n`th burst (counting from 0), as a factor of full level.
    pub fn level(&self, n: usize) -> f32 {
        if self.levels.is_empty() {
            1.0
        } else {
            self.levels[n % self.levels.len()]
        }
    }
}

const fn c(freqs: &'static [f32], steps: &'static [u32]) -> Cadence {
    Cadence { freqs, steps, levels: &[] }
}

/// 10 dB down, as an amplitude factor.
const MINUS_10_DB: f32 = 0.316;

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
        // Australia (Communications Alliance S002). Congestion has the busy
        // cadence with every second burst 10 to 15 dB down; -10 dB is what
        // DAHDI settled on (Asterisk issue DAHTOOL-66, 2014), and Asterisk's
        // indications.conf "au" writes it as 425/375,0/375,420/375,0/375.
        ("AU", Ringback) => AU_RING,
        ("AU", Busy) => c(&[425.0], &[375, 375]),
        ("AU", Congestion) => Cadence { freqs: &[425.0], steps: &[375, 375, 375, 375], levels: &[1.0, MINUS_10_DB] },
        ("NZ", Ringback) => UK_RING,
        ("NZ", Busy) => c(&[400.0], &[500, 500]),
        ("NZ", Congestion) => c(&[400.0], &[250, 250]),
        // Ireland rings like the UK but is busy like the rest of Europe: "Ireland
        // follows the ETSI recommendation for the busy tone, but the ringback tone
        // is the same as that used in the UK" (Wikipedia, "Busy signal"). Its busy
        // and congestion are the CEPT ones below.
        ("GB" | "IE", Ringback) => UK_RING,
        ("GB", Busy) => c(&[400.0], &[375, 375]),
        ("GB", Congestion) => c(&[400.0], &[400, 350, 225, 525]),
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

/// The attention signal an emergency alert sounds with, for `region` (ISO 3166
/// alpha-2): 853 Hz and 960 Hz together, the pair alert systems share, in a
/// cadence (as above, on first, then off) played once.
///
/// The United States and its territories have it in law. Wireless Emergency
/// Alerts' attention signal (47 CFR 10.520) is "one long tone of two (2) seconds,
/// followed by two short tones of one (1) second each, with a half (0.5) second
/// interval between each tone", the whole sequence "repeated twice with a half
/// (0.5) second interval between each repetition": 10.5 s in all.
///
/// Elsewhere there is no pattern to follow. EU-Alert (ETSI TS 102 900 V1.3.1,
/// clause 6.1.2) asks only for "a dedicated alerting indication (audio attention signal
/// and a dedicated vibration cadence)", distinct from the phone's other sounds, and
/// leaves its form to each country, none of which Dolphone has found published; so
/// the rest of the world gets the same two frequencies in four short bursts, about
/// four seconds.
pub fn alert_attention(region: &str) -> Cadence {
    const WEA: Cadence = c(&[853.0, 960.0], &[2000, 500, 1000, 500, 1000, 500, 2000, 500, 1000, 500, 1000, 0]);
    const OTHER: Cadence = c(&[853.0, 960.0], &[900, 150, 900, 150, 900, 150, 900, 150]);
    match region.to_ascii_uppercase().as_str() {
        // The states and the territories WEA covers.
        "US" | "PR" | "GU" | "VI" | "AS" | "MP" => WEA,
        _ => OTHER,
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
    fn busy_and_congestion_differ() {
        for r in ["AU", "NZ", "GB", "IE", "US", "FR", "DE", "IT", "IN", "CN", "HK", "SG", "KR", "BR", "ZA", "XX"] {
            assert_ne!(cadence(r, Progress::Busy), cadence(r, Progress::Congestion), "{r}");
        }
        let au = cadence("AU", Progress::Congestion);
        assert_eq!((au.level(0), au.level(1), au.level(2)), (1.0, MINUS_10_DB, 1.0));
        assert_eq!(cadence("AU", Progress::Busy).level(1), 1.0);
        assert_eq!(cadence("IE", Progress::Ringback), cadence("GB", Progress::Ringback));
        assert_eq!(cadence("IE", Progress::Busy), cadence("XX", Progress::Busy));
    }

    #[test]
    fn cadences_are_well_formed() {
        for r in ["AU", "NZ", "GB", "IE", "US", "FR", "DE", "IT", "JP", "IN", "CN", "HK", "SG", "KR", "BR", "ZA", "XX"]
        {
            for p in [Progress::Ringback, Progress::Busy, Progress::Congestion] {
                let t = cadence(r, p);
                assert!(!t.freqs.is_empty(), "{r} {p:?}");
                assert!(t.steps.len().is_multiple_of(2), "{r} {p:?}: on/off pairs");
                assert!(t.freqs.iter().all(|f| (300.0..700.0).contains(f)), "{r} {p:?}: tones are low and plain");
                assert!((400..=6000).contains(&t.cycle_ms()), "{r} {p:?}");
            }
        }
    }

    #[test]
    fn us_alerts_follow_47_cfr_10_520() {
        let us = alert_attention("us");
        assert_eq!(us.freqs, &[853.0, 960.0]);
        assert_eq!(us.cycle_ms(), 10_500, "two 5 s sequences and the 0.5 s between them");
        let on: Vec<u32> = us.steps.iter().step_by(2).copied().collect();
        assert_eq!(on, [2000, 1000, 1000, 2000, 1000, 1000], "a long tone, then two short ones, twice");
        assert!(us.steps.iter().skip(1).step_by(2).take(5).all(|&off| off == 500), "half a second between tones");
        assert_eq!(alert_attention("PR"), us, "and the territories");
        let au = alert_attention("AU");
        assert_eq!(au.freqs, us.freqs, "the same pair elsewhere");
        assert_eq!(au.cycle_ms(), 4200);
        assert!(au.steps.len().is_multiple_of(2));
    }
}
