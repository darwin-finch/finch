# Phase 3: Daemon-Only Mode + Service Discovery

**Status:** Planning
**Effort:** 2-3 days
**Goal:** Enable daemon to run standalone and advertise via mDNS for auto-discovery

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│          MACHINE 1: iMac Pro (GPU Server)                    │
│                                                               │
│  $ finch daemon --mode daemon-only --bind 0.0.0.0:11435     │
│                                                               │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ Daemon (No REPL)                                       │ │
│  │  • HTTP server only                                    │ │
│  │  • Loads Qwen-7B (GPU accelerated)                     │ │
│  │  • Advertises via mDNS                                 │ │
│  └────────────────────────────────────────────────────────┘ │
│                            │                                  │
│                            ▼                                  │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ mDNS/Bonjour Broadcast                                 │ │
│  │  Service: _finch._tcp.local.                           │ │
│  │  Name: finch-imac-pro                                  │ │
│  │  Port: 11435                                           │ │
│  │  TXT: model=Qwen-7B,device=metal                       │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
                            │
                            │ LAN Broadcast
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│          MACHINE 2: MacBook Air (Client)                     │
│                                                               │
│  $ finch                                                     │
│                                                               │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ CLI Startup                                            │ │
│  │  1. Check local daemon (127.0.0.1:11435)               │ │
│  │  2. Not found? Discover via mDNS                       │ │
│  │  3. Found: finch-imac-pro (192.168.1.100:11435)        │ │
│  │  4. Connect to remote daemon                           │ │
│  └────────────────────────────────────────────────────────┘ │
│                            │                                  │
│                            ▼ HTTP to 192.168.1.100:11435     │
│                   (Uses iMac Pro's GPU)                       │
└─────────────────────────────────────────────────────────────┘
```

## Implementation Steps

### Step 1: Add mDNS dependency (1 hour)

**Update Cargo.toml:**
```toml
[dependencies]
# Service discovery
mdns-sd = "0.11"  # Cross-platform mDNS/DNS-SD
```

**Why mdns-sd:**
- Pure Rust (no C dependencies)
- Cross-platform (macOS, Linux, Windows)
- Active maintenance
- Simple API

### Step 2: Create service discovery module (2-3 hours)

**File:** `src/service/discovery.rs`

```rust
use mdns_sd::{ServiceDaemon, ServiceInfo};
use anyhow::Result;
use std::time::Duration;

pub const SERVICE_TYPE: &str = "_finch._tcp.local.";

/// Service advertisement (server-side)
pub struct ServiceAdvertiser {
    daemon: ServiceDaemon,
}

impl ServiceAdvertiser {
    pub fn new() -> Result<Self> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
        })
    }

    pub fn advertise(&self, config: &AdvertiseConfig) -> Result<()> {
        let hostname = hostname::get()?
            .into_string()
            .unwrap_or_else(|_| "finch".to_string());

        let instance_name = format!("finch-{}", hostname);

        let properties = vec![
            ("model", config.model_name.as_str()),
            ("device", config.device.as_str()),
            ("version", env!("CARGO_PKG_VERSION")),
        ];

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &format!("{}.", hostname),
            (),  // Use default IP
            config.port,
            Some(properties),
        )?;

        self.daemon.register(service_info)?;

        tracing::info!(
            "Advertising service: {} on port {} (model: {}, device: {})",
            instance_name,
            config.port,
            config.model_name,
            config.device
        );

        Ok(())
    }
}

pub struct AdvertiseConfig {
    pub port: u16,
    pub model_name: String,
    pub device: String,
}

/// Service discovery (client-side)
pub struct ServiceDiscoverer {
    daemon: ServiceDaemon,
}

#[derive(Debug, Clone)]
pub struct DiscoveredService {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: String,
    pub device: String,
    pub version: String,
}

impl ServiceDiscoverer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
        })
    }

    pub fn discover(&self, timeout: Duration) -> Result<Vec<DiscoveredService>> {
        use mdns_sd::ServiceEvent;

        let receiver = self.daemon.browse(SERVICE_TYPE)?;
        let deadline = std::time::Instant::now() + timeout;

        let mut services = Vec::new();

        while std::time::Instant::now() < deadline {
            let remaining = deadline - std::time::Instant::now();

            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    services.push(DiscoveredService {
                        name: info.get_fullname().to_string(),
                        host: info.get_hostname().to_string(),
                        port: info.get_port(),
                        model: info.get_property_val_str("model")
                            .unwrap_or("unknown").to_string(),
                        device: info.get_property_val_str("device")
                            .unwrap_or("unknown").to_string(),
                        version: info.get_property_val_str("version")
                            .unwrap_or("unknown").to_string(),
                    });
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        Ok(services)
    }
}
```

### Step 3: Add daemon-only mode (2 hours)

**Update config:**
```toml
[daemon]
mode = "full"  # or "daemon-only"
bind = "127.0.0.1:11435"
advertise = true  # Enable mDNS advertisement
```

**Update daemon startup:**
```rust
// src/daemon/lifecycle.rs

pub async fn start_daemon(config: Config) -> Result<()> {
    let mode = config.daemon.mode.as_str();

    match mode {
        "daemon-only" => {
            info!("Starting in daemon-only mode (no REPL)");

            // Start HTTP server
            let server = start_http_server(&config).await?;

            // Advertise via mDNS if enabled
            if config.daemon.advertise {
                let advertiser = ServiceAdvertiser::new()?;
                advertiser.advertise(&AdvertiseConfig {
                    port: config.daemon.port,
                    model_name: get_model_name(&config),
                    device: config.backend.execution_target.clone(),
                })?;
            }

            // Run forever
            server.await?;
        }
        "full" | _ => {
            // Default: Start daemon + REPL (current behavior)
            start_daemon_and_repl(&config).await?;
        }
    }

    Ok(())
}
```

### Step 4: Add discovery to CLI (2-3 hours)

**REPL initialization with discovery:**
```rust
// src/cli/repl.rs

async fn try_connect_daemon(config: &Config) -> Option<Arc<DaemonClient>> {
    // 1. Try local daemon first
    if let Some(client) = try_connect_local(&config).await {
        return Some(client);
    }

    // 2. If configured to discover, search for remote daemons
    if config.client.auto_discover {
        info!("Local daemon not found, discovering remote daemons...");

        let discoverer = ServiceDiscoverer::new().ok()?;
        let services = discoverer.discover(Duration::from_secs(3)).ok()?;

        if services.is_empty() {
            warn!("No remote daemons discovered");
            return None;
        }

        // Show discovered services
        info!("Discovered {} daemon(s):", services.len());
        for service in &services {
            info!("  • {} ({}:{}) - {} on {}",
                service.name, service.host, service.port,
                service.model, service.device);
        }

        // Connect to first available (or let user choose)
        let service = &services[0];
        let url = format!("http://{}:{}", service.host, service.port);

        if let Some(client) = try_connect_url(&url).await {
            info!("Connected to remote daemon: {}", service.name);
            return Some(client);
        }
    }

    None
}
```

### Step 5: Add CLI commands (1 hour)

**New commands:**
```bash
# Discover available daemons
finch discover

# Output:
# Discovering Finch daemons on local network...
#
# Found 2 daemon(s):
#   • finch-imac-pro (192.168.1.100:11435)
#     Model: Qwen-7B, Device: metal, Version: 0.3.0
#   • finch-desktop (192.168.1.101:11435)
#     Model: Mistral-7B, Device: cuda, Version: 0.3.0

# Connect to specific daemon
finch --remote http://192.168.1.100:11435

# Or let auto-discovery handle it
finch  # Automatically discovers and connects
```

## Configuration

**Server (iMac Pro):**
```toml
[daemon]
mode = "daemon-only"
bind = "0.0.0.0:11435"  # Listen on all interfaces
advertise = true         # Enable mDNS

[backend]
model_family = "Qwen2"
model_size = "Large"  # 7B model
```

**Client (MacBook Air):**
```toml
[client]
use_daemon = true
auto_discover = true  # NEW: Enable mDNS discovery
prefer_local = true   # Try local daemon first
```

## Testing

```bash
# Test 1: Start daemon-only on desktop
# Terminal 1 (iMac Pro):
finch daemon --mode daemon-only --bind 0.0.0.0:11435

# Test 2: Discover from laptop
# Terminal 2 (MacBook Air):
finch discover
# Should show: finch-imac-pro (192.168.1.100:11435)

# Test 3: Auto-connect
finch
# Should automatically discover and connect to iMac Pro

# Test 4: Manual connect
finch --remote http://192.168.1.100:11435
```

## Files to Create/Modify

| File | Type | Description |
|------|------|-------------|
| `Cargo.toml` | MODIFY | Add mdns-sd dependency |
| `src/service/mod.rs` | NEW | Module declaration |
| `src/service/discovery.rs` | NEW | mDNS advertisement and discovery |
| `src/daemon/lifecycle.rs` | MODIFY | Add daemon-only mode |
| `src/cli/repl.rs` | MODIFY | Add auto-discovery to connection logic |
| `src/config/settings.rs` | MODIFY | Add daemon.mode, daemon.advertise, client.auto_discover |
| `src/cli/commands.rs` | MODIFY | Add /discover command |

## Success Criteria

- ✅ Daemon can run in daemon-only mode (no REPL)
- ✅ Daemon advertises via mDNS on LAN
- ✅ CLI can discover remote daemons automatically
- ✅ CLI can connect to remote daemon
- ✅ Queries execute on remote GPU
- ✅ `finch discover` lists all available daemons

## Timeline

- **Day 1:** mDNS module + advertisement (Steps 1-2)
- **Day 2:** Daemon-only mode + discovery (Steps 3-4)
- **Day 3:** CLI commands + testing (Step 5)
