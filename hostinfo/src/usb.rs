//! The USB devices attached to the computer. Windows, macOS and Linux list
//! them through nusb, which reads what the OS already knows; FreeBSD asks
//! `usbconfig`. No device is opened, so nothing here needs more rights than
//! the user has, and a computer that refuses the list yields an empty one.

use crate::model::UsbDevice;

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub fn devices() -> Vec<UsbDevice> {
    use nusb::MaybeFuture;
    let Ok(listed) = nusb::list_devices().wait() else {
        return Vec::new();
    };
    let mut devices: Vec<UsbDevice> = listed
        .map(|device| UsbDevice {
            bus: device.bus_id().to_owned(),
            address: u32::from(device.device_address()),
            vid: device.vendor_id(),
            pid: device.product_id(),
            manufacturer: device.manufacturer_string().unwrap_or_default().to_owned(),
            product: device.product_string().unwrap_or_default().to_owned(),
            class: device.class(),
            speed: device
                .speed()
                .map(speed_name)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect();
    sort(&mut devices);
    devices
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn speed_name(speed: nusb::Speed) -> &'static str {
    match speed {
        nusb::Speed::Low => "low",
        nusb::Speed::Full => "full",
        nusb::Speed::High => "high",
        nusb::Speed::Super => "super",
        nusb::Speed::SuperPlus => "super+",
        _ => "",
    }
}

/// FreeBSD: `usbconfig` prints every device with its descriptor. It needs
/// read access to the USB device nodes, which a normal user has only where
/// the administrator granted it; without it the list is empty.
#[cfg(target_os = "freebsd")]
pub fn devices() -> Vec<UsbDevice> {
    let limit = std::time::Duration::from_secs(5);
    crate::command::output("/usr/sbin/usbconfig", &["dump_device_desc"], limit)
        .map(|text| parse_usbconfig(&text))
        .unwrap_or_default()
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd"
)))]
pub fn devices() -> Vec<UsbDevice> {
    Vec::new()
}

/// The same order each time, so two lists of the same devices are equal.
#[cfg(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    test
))]
fn sort(devices: &mut [UsbDevice]) {
    devices.sort_by(|a, b| (&a.bus, a.address).cmp(&(&b.bus, b.address)));
}

/// The devices in the output of `usbconfig dump_device_desc`. Each device
/// starts with a line such as
/// `ugen0.2: <Logitech USB Receiver> at usbus0, cfg=0 md=HOST spd=FULL (12Mbps) pwr=ON (98mA)`
/// and its descriptor fields follow, indented, as `name = 0x0000 <text>`.
#[cfg(any(target_os = "freebsd", test))]
pub fn parse_usbconfig(text: &str) -> Vec<UsbDevice> {
    let mut devices: Vec<UsbDevice> = Vec::new();
    for line in text.lines() {
        if let Some(device) = usbconfig_header(line) {
            devices.push(device);
        } else if let (Some(device), Some((name, value))) =
            (devices.last_mut(), line.split_once('='))
        {
            usbconfig_field(device, name.trim(), value.trim());
        }
    }
    sort(&mut devices);
    devices
}

/// The device a header line starts: its bus and address from `ugenB.A`,
/// its product name and its speed.
#[cfg(any(target_os = "freebsd", test))]
fn usbconfig_header(line: &str) -> Option<UsbDevice> {
    let (node, rest) = line.strip_prefix("ugen")?.split_once(':')?;
    let (bus, address) = node.split_once('.')?;
    let product = rest
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(product, _)| product.trim());
    let speed = rest
        .split_whitespace()
        .find_map(|word| word.strip_prefix("spd="))
        .map(|speed| match speed {
            "LOW" => "low",
            "FULL" => "full",
            "HIGH" => "high",
            "SUPER" => "super",
            "SUPER+" | "SUPERPLUS" => "super+",
            _ => "",
        });
    Some(UsbDevice {
        bus: bus.parse::<u32>().ok()?.to_string(),
        address: address.parse().ok()?,
        product: product.unwrap_or_default().to_owned(),
        speed: speed.unwrap_or_default().to_owned(),
        ..UsbDevice::default()
    })
}

#[cfg(any(target_os = "freebsd", test))]
fn usbconfig_field(device: &mut UsbDevice, name: &str, value: &str) {
    let number = value
        .split_whitespace()
        .next()
        .and_then(|word| u16::from_str_radix(word.trim_start_matches("0x"), 16).ok());
    let text = value
        .split_once('<')
        .and_then(|(_, rest)| rest.rsplit_once('>'))
        .map(|(text, _)| text.trim())
        .filter(|text| !text.is_empty() && *text != "no string");
    match (name, number) {
        ("idVendor", Some(id)) => device.vid = id,
        ("idProduct", Some(id)) => device.pid = id,
        ("bDeviceClass", Some(class)) => device.class = class.to_le_bytes()[0],
        ("iManufacturer", _) => device.manufacturer = text.unwrap_or_default().to_owned(),
        ("iProduct", _) => {
            if let Some(text) = text {
                device.product = text.to_owned();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USBCONFIG: &str = "\
ugen0.2: <Logitech USB Receiver> at usbus0, cfg=0 md=HOST spd=FULL (12Mbps) pwr=ON (98mA)

  bLength = 0x0012 
  bDescriptorType = 0x0001 
  bcdUSB = 0x0200 
  bDeviceClass = 0x0000  <Probed by interface class>
  bDeviceSubClass = 0x0000 
  bDeviceProtocol = 0x0000 
  bMaxPacketSize0 = 0x0020 
  idVendor = 0x046d 
  idProduct = 0xc52b 
  bcdDevice = 0x2411 
  iManufacturer = 0x0001  <Logitech>
  iProduct = 0x0002  <USB Receiver>
  iSerialNumber = 0x0000  <no string>
  bNumConfigurations = 0x0001 

ugen0.1: <Intel XHCI root HUB> at usbus0, cfg=0 md=HOST spd=SUPER (5.0Gbps) pwr=SAVE (0mA)

  bDeviceClass = 0x0009  <HUB>
  idVendor = 0x0000 
  idProduct = 0x0000 
  iManufacturer = 0x0001  <Intel>
  iProduct = 0x0002  <XHCI root HUB>
";

    #[test]
    fn reads_usbconfig_devices_in_bus_and_address_order() {
        let devices = parse_usbconfig(USBCONFIG);
        assert_eq!(
            devices,
            [
                UsbDevice {
                    bus: "0".into(),
                    address: 1,
                    vid: 0,
                    pid: 0,
                    manufacturer: "Intel".into(),
                    product: "XHCI root HUB".into(),
                    class: 9,
                    speed: "super".into(),
                },
                UsbDevice {
                    bus: "0".into(),
                    address: 2,
                    vid: 0x046d,
                    pid: 0xc52b,
                    manufacturer: "Logitech".into(),
                    product: "USB Receiver".into(),
                    class: 0,
                    speed: "full".into(),
                },
            ]
        );
    }

    #[test]
    fn text_that_is_not_usbconfig_output_has_no_devices() {
        assert!(parse_usbconfig("").is_empty());
        assert!(parse_usbconfig("usbconfig: permission denied\n  idVendor = 0x1\n").is_empty());
    }
}
