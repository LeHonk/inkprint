//! Wall-clock time without an OS: UTC calendar conversion and the SNTP packet format
//! (RFC 4330). Pure functions, so the firmware's RTC and NTP code stay small.

#![no_std]

/// A UTC calendar time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u8,
}

impl DateTime {
    /// Calendar time for seconds since 1970-01-01T00:00:00Z.
    pub fn from_unix(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        // Howard Hinnant's days-from-civil, inverted.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
        Self {
            year,
            month,
            day,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            // 1970-01-01 was a Thursday.
            weekday: ((days + 4).rem_euclid(7)) as u8,
        }
    }

    /// Seconds since 1970-01-01T00:00:00Z. `None` for dates before 1970 or out of range.
    pub fn to_unix(&self) -> Option<u64> {
        if !(1..=12).contains(&self.month) || !(1..=31).contains(&self.day) || self.hour > 23 || self.minute > 59 || self.second > 59 {
            return None;
        }
        let (y, m) = (i64::from(self.year) - i64::from(self.month <= 2), i64::from(self.month));
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let secs = days * 86_400 + i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second);
        u64::try_from(secs).ok()
    }
}

/// SNTP (RFC 4330) client packets.
pub mod sntp {
    /// NTP timestamps count from 1900; Unix time from 1970.
    const NTP_TO_UNIX: u64 = 2_208_988_800;
    pub const PORT: u16 = 123;
    pub const PACKET_LEN: usize = 48;

    /// A client request. `cookie` goes in the transmit timestamp; the server echoes it as
    /// the originate timestamp, which ties the reply to this request.
    pub fn request(cookie: u64) -> [u8; PACKET_LEN] {
        let mut p = [0u8; PACKET_LEN];
        p[0] = 0b00_100_011; // LI 0, version 4, mode 3 (client)
        p[40..48].copy_from_slice(&cookie.to_be_bytes());
        p
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Error {
        Short,
        /// Not a server reply to our request (mode, version or originate mismatch).
        NotOurs,
        /// Stratum 0: kiss-of-death (rate limiting, access denied); try later.
        KissOfDeath,
        /// The server is not synchronised itself.
        Unsynchronised,
    }

    /// The server's transmit time in Unix seconds.
    pub fn parse_reply(reply: &[u8], cookie: u64) -> Result<u64, Error> {
        if reply.len() < PACKET_LEN {
            return Err(Error::Short);
        }
        let (li, version, mode) = (reply[0] >> 6, (reply[0] >> 3) & 7, reply[0] & 7);
        let originate = u64::from_be_bytes(reply[24..32].try_into().unwrap());
        if mode != 4 || !(3..=4).contains(&version) || originate != cookie {
            return Err(Error::NotOurs);
        }
        match reply[1] {
            0 => return Err(Error::KissOfDeath),
            16.. => return Err(Error::Unsynchronised),
            _ => {}
        }
        if li == 3 {
            return Err(Error::Unsynchronised);
        }
        let secs = u64::from(u32::from_be_bytes(reply[40..44].try_into().unwrap()));
        if secs == 0 {
            return Err(Error::Unsynchronised);
        }
        // NTP era 1 starts in 2036: small values are after the wrap (valid until 2104).
        let secs = if secs < 0x8000_0000 { secs + (1 << 32) } else { secs };
        Ok(secs - NTP_TO_UNIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_round_trips() {
        let cases = [
            (0, DateTime { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0, weekday: 4 }),
            // Leap day, and a known Sunday.
            (951_782_400, DateTime { year: 2000, month: 2, day: 29, hour: 0, minute: 0, second: 0, weekday: 2 }),
            (1_792_438_861, DateTime { year: 2026, month: 10, day: 19, hour: 19, minute: 41, second: 1, weekday: 1 }),
            (2_147_483_647, DateTime { year: 2038, month: 1, day: 19, hour: 3, minute: 14, second: 7, weekday: 2 }),
        ];
        for (secs, dt) in cases {
            assert_eq!(DateTime::from_unix(secs), dt, "{secs}");
            assert_eq!(dt.to_unix(), Some(secs), "{dt:?}");
        }
        // Every day for ~130 years round-trips.
        for day in 0..48_000u64 {
            let secs = day * 86_400 + 12_345;
            assert_eq!(DateTime::from_unix(secs).to_unix(), Some(secs));
        }
        assert_eq!(DateTime { month: 13, ..DateTime::from_unix(0) }.to_unix(), None);
    }

    fn reply(stratum: u8, transmit: u32, cookie: u64) -> [u8; 48] {
        let mut p = [0u8; 48];
        p[0] = 0b00_100_100; // LI 0, v4, server
        p[1] = stratum;
        p[24..32].copy_from_slice(&cookie.to_be_bytes());
        p[40..44].copy_from_slice(&transmit.to_be_bytes());
        p
    }

    #[test]
    fn sntp() {
        let req = sntp::request(0xDEAD_BEEF);
        assert_eq!(req[0], 0x23);
        // 2026-10-19T19:41:01Z in NTP seconds.
        let ntp = (1_792_438_861u64 + 2_208_988_800) as u32;
        assert_eq!(sntp::parse_reply(&reply(2, ntp, 0xDEAD_BEEF), 0xDEAD_BEEF), Ok(1_792_438_861));
        assert_eq!(sntp::parse_reply(&reply(2, ntp, 1), 0xDEAD_BEEF), Err(sntp::Error::NotOurs));
        assert_eq!(sntp::parse_reply(&reply(0, ntp, 7), 7), Err(sntp::Error::KissOfDeath));
        assert_eq!(sntp::parse_reply(&reply(16, ntp, 7), 7), Err(sntp::Error::Unsynchronised));
        assert_eq!(sntp::parse_reply(&req, 0), Err(sntp::Error::NotOurs));
        assert_eq!(sntp::parse_reply(&[0; 10], 0), Err(sntp::Error::Short));
        // After the 2036 era wrap: NTP seconds 100 → 2036-02-07T06:29:16Z.
        assert_eq!(sntp::parse_reply(&reply(2, 100, 7), 7), Ok((1u64 << 32) + 100 - 2_208_988_800));
    }
}
