//! BLE transport for the LT200B: discovery, connection, and writing a
//! pre-encoded [`label_core`] print job to the printer's characteristic.

use std::time::Duration;

use async_trait::async_trait;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral};
use label_core::{characteristic_uuid, SERVICE_UUID_PREFIX};

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("bluetooth error: {0}")]
    Bluetooth(#[from] btleplug::Error),
    #[error("no bluetooth adapter found")]
    NoAdapter,
    #[error("printer {0} not found (is it powered on and in range?)")]
    NotFound(String),
    #[error("{0} does not look like a compatible LT200B printer (no be3dd650... service)")]
    NotCompatible(String),
}

/// A printer seen during a scan.
#[derive(Debug, Clone)]
pub struct DiscoveredPrinter {
    /// Platform BLE identifier: a MAC address on Linux/Windows, an opaque
    /// per-app UUID on macOS.
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

/// Scans for nearby BLE peripherals for `timeout` and returns every named
/// device seen. Compatibility (the `be3dd650...` service) isn't known until
/// connect time, since most OSes don't expose advertised service UUIDs for
/// unpaired devices without a filter that also hides unnamed accessories.
pub async fn scan(timeout: Duration) -> Result<Vec<DiscoveredPrinter>, DeviceError> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters.into_iter().next().ok_or(DeviceError::NoAdapter)?;

    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(timeout).await;
    let peripherals = adapter.peripherals().await?;
    adapter.stop_scan().await?;

    let mut found = Vec::new();
    for p in peripherals {
        let Some(props) = p.properties().await? else {
            continue;
        };
        if props.local_name.is_none() {
            continue;
        }
        found.push(DiscoveredPrinter {
            id: p.id().to_string(),
            name: props.local_name,
            rssi: props.rssi,
        });
    }
    Ok(found)
}

/// Something a print job's chunks can be written to, in order. Lets
/// [`print_job`] run against either a real printer or a test double.
#[async_trait]
pub trait Transport: Send {
    async fn write(&mut self, chunk: &[u8]) -> Result<(), DeviceError>;
}

/// Writes every chunk of an already-encoded job to `transport`, in order.
pub async fn print_job<T: Transport + ?Sized>(
    transport: &mut T,
    job: &[Vec<u8>],
) -> Result<(), DeviceError> {
    for chunk in job {
        transport.write(chunk).await?;
    }
    Ok(())
}

/// A connected LT200B, ready to receive print jobs.
pub struct Printer {
    peripheral: Peripheral,
    char_uuid: uuid::Uuid,
}

impl Printer {
    /// Connects to `id` (as returned by [`scan`], or a platform BLE
    /// address/UUID the caller already knows) and locates the printer's
    /// data characteristic.
    pub async fn connect(id: &str) -> Result<Self, DeviceError> {
        let manager = Manager::new().await?;
        let adapters = manager.adapters().await?;
        let adapter = adapters.into_iter().next().ok_or(DeviceError::NoAdapter)?;

        // peripherals() only returns previously-seen devices; scan briefly
        // if we haven't seen this one yet.
        adapter.start_scan(ScanFilter::default()).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        adapter.stop_scan().await?;

        let peripherals = adapter.peripherals().await?;
        let peripheral = {
            let mut found = None;
            for p in peripherals {
                if p.id().to_string() == id {
                    found = Some(p);
                    break;
                }
            }
            found.ok_or_else(|| DeviceError::NotFound(id.to_string()))?
        };

        peripheral.connect().await?;
        peripheral.discover_services().await?;

        let services = peripheral.services();
        let second_segment = services
            .iter()
            .find_map(|s| {
                let s = s.uuid.to_string();
                let mut parts = s.splitn(2, '-');
                let first = parts.next()?;
                if first.eq_ignore_ascii_case(SERVICE_UUID_PREFIX) {
                    parts.next().and_then(|rest| rest.splitn(4, '-').next().map(String::from))
                } else {
                    None
                }
            })
            .ok_or_else(|| DeviceError::NotCompatible(id.to_string()))?;

        let char_uuid = uuid::Uuid::parse_str(&characteristic_uuid(&second_segment))
            .map_err(|_| DeviceError::NotCompatible(id.to_string()))?;

        Ok(Self { peripheral, char_uuid })
    }

    /// Prints an already-encoded job (see [`label_core::create_job`]).
    pub async fn print_job(&self, job: &[Vec<u8>]) -> Result<(), DeviceError> {
        let characteristics = self.peripheral.characteristics();
        let characteristic = characteristics
            .iter()
            .find(|c| c.uuid == self.char_uuid)
            .ok_or_else(|| DeviceError::NotCompatible(self.char_uuid.to_string()))?;

        for chunk in job {
            self.peripheral
                .write(characteristic, chunk, WriteType::WithoutResponse)
                .await?;
        }
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<(), DeviceError> {
        self.peripheral.disconnect().await?;
        Ok(())
    }
}

/// A [`Transport`] that records every write instead of talking to
/// hardware, for protocol-level tests.
#[derive(Default)]
pub struct RecordingTransport {
    pub writes: Vec<Vec<u8>>,
}

#[async_trait]
impl Transport for RecordingTransport {
    async fn write(&mut self, chunk: &[u8]) -> Result<(), DeviceError> {
        self.writes.push(chunk.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use label_core::{create_job, Raster};

    #[tokio::test]
    async fn print_job_writes_every_chunk_in_order() {
        let raster = Raster::new(32, 4, vec![false; 32 * 4]);
        let job = create_job(&raster);
        let mut transport = RecordingTransport::default();
        print_job(&mut transport, &job).await.unwrap();
        assert_eq!(transport.writes.len(), job.len());
        assert_eq!(transport.writes, job);
    }
}
