//! Windows Setup answer file (`Autounattend.xml`).
//!
//! Schema: Windows Unattended Setup Reference, `Microsoft-Windows-Setup`
//! DiskConfiguration / ImageInstall / UserData, `Microsoft-Windows-PnpCustomizationsWinPE`
//! DriverPaths, and `Microsoft-Windows-Shell-Setup` OOBE / UserAccounts /
//! FirstLogonCommands (Microsoft Learn, retrieved 2026-11-06).
//! TODO(verify): image `/IMAGE/NAME` on the user's Windows 11 ARM64 ISO, and
//! which optical drive letter Setup assigns to `iso.tools`.

use crate::secret::Secret;

/// Inputs for [`render`]. Password and product key are never written to logs.
pub struct UnattendSpec<'a> {
    /// Local account name (ASCII).
    pub user: &'a str,
    /// Account password (Keychain / env / file).
    pub password: &'a Secret,
    /// Optional retail/OEM key; empty means omit `<Key>` text.
    pub product_key: Option<&'a Secret>,
    /// `/IMAGE/NAME` (for example `Windows 11 Pro`).
    pub edition: &'a str,
    /// NetBIOS computer name.
    pub computer_name: &'a str,
}

/// UTF-8 `Autounattend.xml`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(user = spec.user, edition = spec.edition))]
pub fn render(spec: &UnattendSpec<'_>) -> String {
    let user = xml_escape(spec.user);
    let computer = xml_escape(truncate_netbios(spec.computer_name));
    let edition = xml_escape(spec.edition);
    let password = encode_password(spec.password.expose());
    let key = spec
        .product_key
        .filter(|key| !key.is_empty())
        .map(|key| xml_escape(key.expose()))
        .unwrap_or_default();
    let key_elem = if key.is_empty() {
        String::new()
    } else {
        format!("            <Key>{key}</Key>\n")
    };
    tracing::info!(
        target: "ternvale::cli",
        user = spec.user,
        edition = spec.edition,
        has_product_key = !key.is_empty(),
        "rendered Autounattend.xml (secrets redacted)"
    );
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <SetupUILanguage>
        <UILanguage>en-US</UILanguage>
      </SetupUILanguage>
      <InputLocale>en-US</InputLocale>
      <SystemLocale>en-US</SystemLocale>
      <UILanguage>en-US</UILanguage>
      <UserLocale>en-US</UserLocale>
    </component>
    <component name="Microsoft-Windows-PnpCustomizationsWinPE" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <DriverPaths>
{driver_paths}
      </DriverPaths>
    </component>
    <component name="Microsoft-Windows-Setup" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <DiskConfiguration>
        <WillShowUI>OnError</WillShowUI>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add">
              <Order>1</Order>
              <Type>EFI</Type>
              <Size>260</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>2</Order>
              <Type>MSR</Type>
              <Size>16</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>3</Order>
              <Type>Primary</Type>
              <Extend>true</Extend>
            </CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add">
              <Order>1</Order>
              <PartitionID>1</PartitionID>
              <Format>FAT32</Format>
              <Label>System</Label>
            </ModifyPartition>
            <ModifyPartition wcm:action="add">
              <Order>2</Order>
              <PartitionID>2</PartitionID>
            </ModifyPartition>
            <ModifyPartition wcm:action="add">
              <Order>3</Order>
              <PartitionID>3</PartitionID>
              <Format>NTFS</Format>
              <Label>Windows</Label>
              <Letter>C</Letter>
            </ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall>
        <OSImage>
          <InstallTo>
            <DiskID>0</DiskID>
            <PartitionID>3</PartitionID>
          </InstallTo>
          <InstallToAvailablePartition>false</InstallToAvailablePartition>
          <WillShowUI>OnError</WillShowUI>
          <InstallFrom>
            <MetaData wcm:action="add">
              <Key>/IMAGE/NAME</Key>
              <Value>{edition}</Value>
            </MetaData>
          </InstallFrom>
        </OSImage>
      </ImageInstall>
      <UserData>
        <AcceptEula>true</AcceptEula>
        <ProductKey>
{key_elem}          <WillShowUI>Never</WillShowUI>
        </ProductKey>
      </UserData>
    </component>
  </settings>
  <settings pass="specialize">
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <ComputerName>{computer}</ComputerName>
      <TimeZone>UTC</TimeZone>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
        <NetworkLocation>Work</NetworkLocation>
        <ProtectYourPC>3</ProtectYourPC>
        <SkipMachineOOBE>true</SkipMachineOOBE>
        <SkipUserOOBE>true</SkipUserOOBE>
      </OOBE>
      <UserAccounts>
        <LocalAccounts>
          <LocalAccount wcm:action="add">
            <Name>{user}</Name>
            <Group>Administrators</Group>
            <DisplayName>{user}</DisplayName>
            <Password>
              <Value>{password}</Value>
              <PlainText>false</PlainText>
            </Password>
          </LocalAccount>
        </LocalAccounts>
      </UserAccounts>
      <AutoLogon>
        <Enabled>true</Enabled>
        <Username>{user}</Username>
        <Password>
          <Value>{password}</Value>
          <PlainText>false</PlainText>
        </Password>
        <LogonCount>1</LogonCount>
      </AutoLogon>
      <FirstLogonCommands>
        <SynchronousCommand wcm:action="add">
          <Order>1</Order>
          <Description>Install Ternvale guest tools</Description>
          <CommandLine>powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "foreach ($d in Get-PSDrive -PSProvider FileSystem) {{ $p = Join-Path $d.Root 'install.ps1'; if (Test-Path -LiteralPath $p) {{ &amp; $p; break }} }}"</CommandLine>
        </SynchronousCommand>
      </FirstLogonCommands>
    </component>
  </settings>
</unattend>
"#,
        driver_paths = DRIVER_PATHS
    )
}

/// DriverPaths for viostor / NetKVM on every likely optical letter.
///
/// TODO(verify): Windows 11 ARM64 Setup letter assignment with three CDs.
const DRIVER_PATHS: &str = r#"        <PathAndCredentials wcm:action="add" wcm:keyValue="1">
          <Path>D:\virtio-win\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="2">
          <Path>E:\virtio-win\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="3">
          <Path>F:\virtio-win\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="4">
          <Path>D:\drivers\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="5">
          <Path>E:\drivers\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="6">
          <Path>D:\virtio-win\NetKVM\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="7">
          <Path>E:\virtio-win\NetKVM\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="8">
          <Path>F:\virtio-win\NetKVM\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="9">
          <Path>D:\viostor\w11\ARM64</Path>
        </PathAndCredentials>
        <PathAndCredentials wcm:action="add" wcm:keyValue="10">
          <Path>E:\viostor\w11\ARM64</Path>
        </PathAndCredentials>"#;

/// Microsoft encoded password: Base64(UTF-16LE(password + "Password")).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn encode_password(plain: &str) -> String {
    let mut units: Vec<u16> = plain.encode_utf16().collect();
    units.extend("Password".encode_utf16());
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64_encode(&bytes)
}

fn truncate_netbios(name: &str) -> &str {
    let ascii = name
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_alphanumeric() || *ch == '-')
        .last()
        .map(|(i, ch)| i + ch.len_utf8())
        .unwrap_or(0);
    let trimmed = &name[..ascii];
    if trimmed.len() <= 15 {
        if trimmed.is_empty() {
            "TERNVALE"
        } else {
            trimmed
        }
    } else {
        &trimmed[..15]
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes.get(i + 1).copied();
        let b2 = bytes.get(i + 2).copied();
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        match (b1, b2) {
            (Some(b1), Some(b2)) => {
                out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
                out.push(TABLE[(b2 & 0x3f) as usize] as char);
            }
            (Some(b1), None) => {
                out.push(TABLE[((b1 & 0x0f) << 2) as usize] as char);
                out.push('=');
            }
            (None, _) => {
                out.push('=');
                out.push('=');
            }
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_does_not_contain_the_plaintext_password() {
        let password = Secret::new("TestP@ssw0rd!".into());
        let xml = render(&UnattendSpec {
            user: "Ternvale",
            password: &password,
            product_key: None,
            edition: "Windows 11 Pro",
            computer_name: "winvm",
        });
        assert!(xml.contains("Autounattend") || xml.contains("unattend"));
        assert!(!xml.contains("TestP@ssw0rd!"), "plaintext password leaked");
        assert!(xml.contains("<PlainText>false</PlainText>"));
        assert!(xml.contains("viostor\\w11\\ARM64"));
        assert!(xml.contains("Type>EFI"));
        assert!(xml.contains("install.ps1"));
        assert!(
            !xml.contains("<ProductKey>\n            <Key>"),
            "empty product key must not emit a Key value"
        );
    }

    #[test]
    fn encoded_password_is_not_the_plain_string() {
        let encoded = encode_password("secret");
        assert!(!encoded.contains("secret"));
        assert!(
            encoded.ends_with('=')
                || encoded
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
        );
    }
}
