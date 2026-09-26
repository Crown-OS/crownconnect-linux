//! The laptop battery, from UPower's aggregate display device.

use futures_util::stream::select;
use futures_util::StreamExt;
use llts_signaling::state::Battery;

use crate::state::{LocalState, StateSink};
use crate::util::percent::whole_percent;

const CHARGING: u32 = 1;
const DISCHARGING: u32 = 2;
const FULLY_CHARGED: u32 = 4;
const PENDING_CHARGE: u32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum BatteryError {
    #[error("UPower: {0}")]
    Bus(#[from] zbus::Error),
    #[error("this computer has no battery")]
    NoBattery,
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait DisplayDevice {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
}

/// Publishes the battery now and on every UPower change, until the runtime goes away.
///
/// # Errors
///
/// Fails without a system bus, without UPower or on a computer with no battery.
pub async fn run(sink: StateSink) -> Result<(), BatteryError> {
    let connection = zbus::Connection::system().await?;
    let device = DisplayDeviceProxy::new(&connection).await?;
    if !device.is_present().await? {
        return Err(BatteryError::NoBattery);
    }
    let percentage = device.receive_percentage_changed().await.map(drop);
    let state = device.receive_state_changed().await.map(drop);
    let time_to_empty = device.receive_time_to_empty_changed().await.map(drop);
    let mut changes = select(select(percentage, state), time_to_empty);
    loop {
        let battery = battery_from_upower(
            device.percentage().await?,
            device.state().await?,
            device.time_to_empty().await?,
        );
        if !sink.publish(LocalState::Battery(battery)).await || changes.next().await.is_none() {
            return Ok(());
        }
    }
}

fn battery_from_upower(percentage: f64, state: u32, time_to_empty_seconds: i64) -> Battery {
    let discharging = state == DISCHARGING;
    Battery {
        percent: whole_percent(percentage),
        charging: matches!(state, CHARGING | FULLY_CHARGED | PENDING_CHARGE),
        time_to_empty_min: (discharging && time_to_empty_seconds > 0)
            .then(|| u16::try_from(time_to_empty_seconds / 60).unwrap_or(u16::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discharging_battery_reports_minutes_left() {
        assert_eq!(
            battery_from_upower(41.6, DISCHARGING, 5_430),
            Battery {
                percent: 42,
                charging: false,
                time_to_empty_min: Some(90),
            }
        );
    }

    #[test]
    fn a_plugged_in_battery_reports_charging_without_an_estimate() {
        for state in [CHARGING, FULLY_CHARGED, PENDING_CHARGE] {
            let battery = battery_from_upower(100.0, state, 3_600);
            assert!(battery.charging);
            assert_eq!(battery.time_to_empty_min, None);
        }
    }

    #[test]
    fn out_of_range_readings_are_clamped() {
        assert_eq!(battery_from_upower(180.0, DISCHARGING, 0).percent, 100);
        assert_eq!(
            battery_from_upower(-3.0, DISCHARGING, -1).time_to_empty_min,
            None
        );
        assert_eq!(
            battery_from_upower(5.0, DISCHARGING, i64::MAX).time_to_empty_min,
            Some(u16::MAX)
        );
    }
}
