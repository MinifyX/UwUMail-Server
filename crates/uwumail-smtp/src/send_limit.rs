//! How many people one account may send to per hour and per day (security audit 2026-11 M-02).
//!
//! A stolen password or a hijacked mail program should not turn a mailbox into a spam cannon that
//! gets the whole server blocklisted. Every recipient of every submission counts, from every door
//! (SMTP submission, JMAP and the webmail, calendar invitations by mail), because they all go
//! through [`crate::Smtp::submit`]. The counts live in memory: a restart forgets them, which costs
//! at most one more day's worth, and they need no table.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

/// Which limit a submission would go over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
}

impl Window {
    pub fn name(self) -> &'static str {
        match self {
            Window::Hour => "hour",
            Window::Day => "day",
        }
    }
}

#[derive(Default)]
pub(crate) struct SendLimits {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Per account, recipients per minute (the minute since the epoch), oldest first and at most a
    /// day's worth: at most 1,440 entries for an account that sends every minute.
    accounts: HashMap<i64, VecDeque<(u64, usize)>>,
    /// When accounts that sent nothing for a day were last forgotten.
    swept_at: u64,
}

impl SendLimits {
    /// Counts `recipients` for `account` at `now` (seconds since the epoch) if they fit into both
    /// limits; 0 means no limit. Otherwise counts nothing and says which limit is in the way.
    pub fn take(
        &self,
        account: i64,
        recipients: usize,
        per_hour: usize,
        per_day: usize,
        now: u64,
    ) -> Result<(), Window> {
        let mut state = self.state.lock().expect("send limits poisoned");
        if now.saturating_sub(state.swept_at) >= HOUR {
            state.swept_at = now;
            state.accounts.retain(|_, sent| {
                forget_old(sent, now);
                !sent.is_empty()
            });
        }
        let sent = state.accounts.entry(account).or_default();
        forget_old(sent, now);
        let hour_start = now.saturating_sub(HOUR) / MINUTE;
        let (mut in_hour, mut in_day) = (0usize, 0usize);
        for &(minute, count) in sent.iter() {
            in_day += count;
            if minute > hour_start {
                in_hour += count;
            }
        }
        if per_hour > 0 && in_hour.saturating_add(recipients) > per_hour {
            return Err(Window::Hour);
        }
        if per_day > 0 && in_day.saturating_add(recipients) > per_day {
            return Err(Window::Day);
        }
        let minute = now / MINUTE;
        match sent.back_mut() {
            Some((last, count)) if *last == minute => *count += recipients,
            _ => sent.push_back((minute, recipients)),
        }
        Ok(())
    }
}

fn forget_old(sent: &mut VecDeque<(u64, usize)>, now: u64) {
    let day_start = now.saturating_sub(DAY) / MINUTE;
    while sent.front().is_some_and(|&(minute, _)| minute <= day_start) {
        sent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;

    #[test]
    fn an_hour_and_a_day_are_counted_apart_and_per_account() {
        let limits = SendLimits::default();
        assert_eq!(limits.take(1, 6, 10, 25, T0), Ok(()));
        assert_eq!(limits.take(1, 4, 10, 25, T0 + 30), Ok(()));
        // The hour is full; what does not fit is not counted either.
        assert_eq!(limits.take(1, 1, 10, 25, T0 + 60), Err(Window::Hour));
        // Another account has its own budget.
        assert_eq!(limits.take(2, 10, 10, 25, T0 + 60), Ok(()));
        // An hour later there is room again, until the day is full.
        assert_eq!(limits.take(1, 10, 10, 25, T0 + HOUR + 60), Ok(()));
        assert_eq!(limits.take(1, 6, 10, 25, T0 + 2 * HOUR + 60), Err(Window::Day));
        assert_eq!(limits.take(1, 5, 10, 25, T0 + 2 * HOUR + 60), Ok(()));
        assert_eq!(limits.take(1, 1, 10, 25, T0 + 3 * HOUR + 60), Err(Window::Day));
        // A day after the first message its recipients no longer count.
        assert_eq!(limits.take(1, 6, 10, 25, T0 + DAY + 60), Ok(()));
    }

    #[test]
    fn zero_means_no_limit_and_quiet_accounts_are_forgotten() {
        let limits = SendLimits::default();
        assert_eq!(limits.take(1, 1_000_000, 0, 0, T0), Ok(()));
        assert_eq!(limits.take(2, 1, 0, 0, T0), Ok(()));
        assert_eq!(limits.state.lock().unwrap().accounts.len(), 2);
        assert_eq!(limits.take(3, 1, 0, 0, T0 + DAY + HOUR), Ok(()));
        assert_eq!(limits.state.lock().unwrap().accounts.len(), 1);
    }
}
