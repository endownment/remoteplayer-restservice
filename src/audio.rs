//! Audio outputs as mpv sees them, grouped by sound card with a recommended
//! `--audio-device` value per card.
//!
//! mpv's list comes from ALSA's device hints (the same list as `aplay -L`), so
//! for every card it contains many PCM aliases. Only some of them work on a
//! Raspberry Pi: the vc4 HDMI driver accepts IEC958 frames only, which is why
//! `hdmi:CARD=vc4hdmi` works while `hw:`/`plughw:` on that card fail.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mpv::{MpvClient, MpvError};

const HELP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioOutputKind {
    Hdmi,
    Analog,
    Spdif,
    Usb,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AudioOutput {
    pub card: String,
    pub kind: AudioOutputKind,
    pub description: String,
    pub recommended: String,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceListSource {
    /// Read from the running mpv over IPC.
    MpvIpc,
    /// mpv was not reachable; `mpv --audio-device=help` was run instead.
    MpvHelp,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AudioDeviceList {
    pub source: DeviceListSource,
    pub current: Option<String>,
    pub outputs: Vec<AudioOutput>,
    pub other: Vec<AudioDevice>,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error(transparent)]
    Mpv(#[from] MpvError),
    #[error("cannot run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{0} did not finish in time")]
    Timeout(String),
}

pub struct AudioInventory {
    mpv_executable: PathBuf,
    asound_root: PathBuf,
}

impl AudioInventory {
    pub fn new(mpv_executable: PathBuf) -> AudioInventory {
        AudioInventory { mpv_executable, asound_root: PathBuf::from("/proc/asound") }
    }

    pub async fn list(&self, client: &MpvClient, configured_device: &str) -> Result<AudioDeviceList, AudioError> {
        let (devices, source, current): (Vec<AudioDevice>, DeviceListSource, Option<String>) = if client.is_connected()
        {
            let devices: Vec<AudioDevice> = client.get_property("audio-device-list").await?;
            let current: Option<String> = client.get_property_optional("audio-device").await?;
            (devices, DeviceListSource::MpvIpc, current)
        } else {
            (self.devices_from_help().await?, DeviceListSource::MpvHelp, Some(configured_device.to_string()))
        };
        let (outputs, other): (Vec<AudioOutput>, Vec<AudioDevice>) = group_by_card(devices, &self.asound_root);
        Ok(AudioDeviceList { source, current, outputs, other })
    }

    async fn devices_from_help(&self) -> Result<Vec<AudioDevice>, AudioError> {
        let command_name: String = self.mpv_executable.display().to_string();
        let mut command: tokio::process::Command = tokio::process::Command::new(&self.mpv_executable);
        command.args(["--no-config", "--audio-device=help"]).kill_on_drop(true);
        let output: std::process::Output = tokio::time::timeout(HELP_TIMEOUT, command.output())
            .await
            .map_err(|_elapsed: tokio::time::error::Elapsed| AudioError::Timeout(command_name.clone()))?
            .map_err(|source: std::io::Error| AudioError::Spawn { command: command_name.clone(), source })?;
        Ok(parse_help_output(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// Parses lines like `  'alsa/hdmi:CARD=vc4hdmi,DEV=0' (vc4-hdmi, MAI PCM i2s-hifi-0)`.
pub fn parse_help_output(text: &str) -> Vec<AudioDevice> {
    text.lines()
        .filter_map(|line: &str| {
            let trimmed: &str = line.trim();
            let rest: &str = trimmed.strip_prefix('\'')?;
            let (name, after_name): (&str, &str) = rest.split_once('\'')?;
            let description: &str = after_name.trim().strip_prefix('(')?.strip_suffix(')')?;
            Some(AudioDevice { name: name.to_string(), description: description.to_string() })
        })
        .collect()
}

struct AlsaDeviceName {
    pcm: String,
    card: String,
}

fn parse_alsa_name(name: &str) -> Option<AlsaDeviceName> {
    let alsa_part: &str = name.strip_prefix("alsa/")?;
    let (pcm, arguments): (&str, &str) = alsa_part.split_once(':')?;
    let card: &str = arguments.split(',').find_map(|argument: &str| argument.strip_prefix("CARD="))?;
    Some(AlsaDeviceName { pcm: pcm.to_string(), card: card.to_string() })
}

fn classify_card(card: &str, descriptions: &[String], asound_root: &Path) -> AudioOutputKind {
    let card_lower: String = card.to_ascii_lowercase();
    let description_lower: String = descriptions.join(" ").to_ascii_lowercase();
    if card_lower.contains("hdmi") {
        return AudioOutputKind::Hdmi;
    }
    if asound_root.join(card).join("usbid").exists() {
        return AudioOutputKind::Usb;
    }
    // S/PDIF HATs (HiFiBerry Digi, JustBoom Digi...) are recognisable only by their driver name.
    let spdif_markers: [&str; 4] = ["spdif", "s/pdif", "_digi", "toslink"];
    if spdif_markers.iter().any(|marker: &&str| card_lower.contains(marker) || description_lower.contains(marker)) {
        return AudioOutputKind::Spdif;
    }
    if card_lower == "headphones" || card_lower.contains("bcm2835") {
        return AudioOutputKind::Analog;
    }
    AudioOutputKind::Other
}

fn preferred_pcms(kind: AudioOutputKind) -> &'static [&'static str] {
    match kind {
        AudioOutputKind::Hdmi => &["hdmi", "sysdefault", "plughw"],
        AudioOutputKind::Spdif => &["iec958", "sysdefault", "plughw"],
        AudioOutputKind::Analog | AudioOutputKind::Usb | AudioOutputKind::Other => {
            &["plughw", "sysdefault", "default", "front"]
        }
    }
}

fn group_by_card(devices: Vec<AudioDevice>, asound_root: &Path) -> (Vec<AudioOutput>, Vec<AudioDevice>) {
    let mut cards: BTreeMap<String, Vec<(AlsaDeviceName, AudioDevice)>> = BTreeMap::new();
    let mut other: Vec<AudioDevice> = Vec::new();
    for device in devices {
        match parse_alsa_name(&device.name) {
            Some(parsed) => cards.entry(parsed.card.clone()).or_default().push((parsed, device)),
            None => other.push(device),
        }
    }

    let outputs: Vec<AudioOutput> = cards
        .into_iter()
        .map(|(card, members): (String, Vec<(AlsaDeviceName, AudioDevice)>)| build_output(card, members, asound_root))
        .collect();
    (outputs, other)
}

fn build_output(card: String, members: Vec<(AlsaDeviceName, AudioDevice)>, asound_root: &Path) -> AudioOutput {
    let descriptions: Vec<String> =
        members.iter().map(|(_, device): &(AlsaDeviceName, AudioDevice)| device.description.clone()).collect();
    let kind: AudioOutputKind = classify_card(&card, &descriptions, asound_root);
    let recommended_index: usize = preferred_pcms(kind)
        .iter()
        .find_map(|wanted: &&str| {
            members.iter().position(|(parsed, _): &(AlsaDeviceName, AudioDevice)| parsed.pcm == *wanted)
        })
        .unwrap_or(0);
    let recommended: &AudioDevice = &members[recommended_index].1;
    // ALSA descriptions are often two lines ("card, pcm\nlong name").
    let description: String = recommended.description.replace('\n', " / ");
    let mut alternatives: Vec<String> = members
        .iter()
        .enumerate()
        .filter(|(index, _): &(usize, &(AlsaDeviceName, AudioDevice))| *index != recommended_index)
        .map(|(_, (_, device)): (usize, &(AlsaDeviceName, AudioDevice))| device.name.clone())
        .collect();
    alternatives.sort();
    AudioOutput { card, kind, description, recommended: recommended.name.clone(), alternatives }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RASPBERRY_PI_HELP: &str = "List of detected audio devices:
  'auto' (Autoselect device)
  'alsa' (Default (alsa))
  'alsa/default:CARD=vc4hdmi' (vc4-hdmi, MAI PCM i2s-hifi-0/Default Audio Device)
  'alsa/sysdefault:CARD=vc4hdmi' (vc4-hdmi, MAI PCM i2s-hifi-0/Default Audio Device)
  'alsa/hdmi:CARD=vc4hdmi,DEV=0' (vc4-hdmi, MAI PCM i2s-hifi-0/HDMI Audio Output)
  'alsa/plughw:CARD=vc4hdmi,DEV=0' (vc4-hdmi, MAI PCM i2s-hifi-0/Hardware device with all software conversions)
  'alsa/default:CARD=Headphones' (bcm2835 Headphones, bcm2835 Headphones/Default Audio Device)
  'alsa/plughw:CARD=Headphones,DEV=0' (bcm2835 Headphones, bcm2835 Headphones/Hardware device with all software conversions)
  'alsa/iec958:CARD=sndrpihifiberry,DEV=0' (snd_rpi_hifiberry_digi, HifiBerry Digi HiFi wm8804-spdif-0/IEC958 (S/PDIF) Digital Audio Output)
  'alsa/sysdefault:CARD=sndrpihifiberry' (snd_rpi_hifiberry_digi, HifiBerry Digi HiFi wm8804-spdif-0/Default Audio Device)
";

    #[test]
    fn help_output_is_parsed_line_by_line() {
        let devices: Vec<AudioDevice> = parse_help_output(RASPBERRY_PI_HELP);
        assert_eq!(devices.len(), 10);
        assert_eq!(devices[0], AudioDevice { name: "auto".to_string(), description: "Autoselect device".to_string() });
        assert_eq!(devices[1].description, "Default (alsa)");
    }

    #[test]
    fn raspberry_pi_cards_get_working_recommendations() {
        let devices: Vec<AudioDevice> = parse_help_output(RASPBERRY_PI_HELP);
        let (outputs, other): (Vec<AudioOutput>, Vec<AudioDevice>) = group_by_card(devices, Path::new("/nonexistent"));
        let summary: Vec<(&str, AudioOutputKind, &str)> = outputs
            .iter()
            .map(|output: &AudioOutput| (output.card.as_str(), output.kind, output.recommended.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Headphones", AudioOutputKind::Analog, "alsa/plughw:CARD=Headphones,DEV=0"),
                ("sndrpihifiberry", AudioOutputKind::Spdif, "alsa/iec958:CARD=sndrpihifiberry,DEV=0"),
                ("vc4hdmi", AudioOutputKind::Hdmi, "alsa/hdmi:CARD=vc4hdmi,DEV=0"),
            ]
        );
        assert_eq!(other.len(), 2);
    }
}
