// System monitoring - memory usage, CPU, etc.

use sysinfo::System;

/// Memory usage information
#[derive(Debug, Clone)]
pub struct MemoryInfo {
    /// Total system RAM in bytes
    pub total_memory: u64,
    /// Available system RAM in bytes
    pub available_memory: u64,
    /// Used system RAM in bytes
    pub used_memory: u64,
    /// Current process memory usage in bytes
    pub process_memory: u64,
}

impl MemoryInfo {
    /// Get current memory information
    pub fn current() -> Self {
        let mut system = System::new_all();
        system.refresh_all();

        let total = system.total_memory();
        let available = system.available_memory();
        let used = system.used_memory();

        // Get current process memory
        let pid = sysinfo::get_current_pid().unwrap();
        let process_memory = system.process(pid).map(|p| p.memory()).unwrap_or(0);

        Self {
            total_memory: total,
            available_memory: available,
            used_memory: used,
            process_memory,
        }
    }

    /// Format total memory as human-readable string
    pub fn total_gb(&self) -> f64 {
        self.total_memory as f64 / 1_073_741_824.0 // bytes to GB
    }

    /// Format available memory as human-readable string
    pub fn available_gb(&self) -> f64 {
        self.available_memory as f64 / 1_073_741_824.0
    }

    /// Format used memory as human-readable string
    pub fn used_gb(&self) -> f64 {
        self.used_memory as f64 / 1_073_741_824.0
    }

    /// Format process memory as human-readable string
    pub fn process_mb(&self) -> f64 {
        self.process_memory as f64 / 1_048_576.0 // bytes to MB
    }

    /// Get memory usage percentage
    pub fn usage_percent(&self) -> f64 {
        (self.used_memory as f64 / self.total_memory as f64) * 100.0
    }

    /// Check if memory is critically low (< 10% available)
    pub fn is_critical(&self) -> bool {
        self.available_memory < self.total_memory / 10
    }

    /// Check if memory is low (< 20% available)
    pub fn is_low(&self) -> bool {
        self.available_memory < self.total_memory / 5
    }

    /// Format as status line
    pub fn format_status(&self) -> String {
        format!(
            "Memory: {:.1}GB / {:.1}GB ({:.0}%) | Process: {:.0}MB",
            self.used_gb(),
            self.total_gb(),
            self.usage_percent(),
            self.process_mb()
        )
    }

    /// Format with warning if low
    pub fn format_with_warning(&self) -> String {
        let status = self.format_status();
        if self.is_critical() {
            format!("⚠️  {} (CRITICAL)", status)
        } else if self.is_low() {
            format!("⚠️  {} (LOW)", status)
        } else {
            status
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_info() {
        let info = MemoryInfo::current();

        // Basic sanity checks
        assert!(info.total_memory > 0);
        assert!(info.available_memory <= info.total_memory);
        assert!(info.used_memory <= info.total_memory);
        assert!(info.process_memory > 0);
    }

    #[test]
    fn test_format() {
        let info = MemoryInfo::current();
        let status = info.format_status();

        // Should contain expected components
        assert!(status.contains("Memory:"));
        assert!(status.contains("GB"));
        assert!(status.contains("Process:"));
        assert!(status.contains("MB"));
    }

    #[test]
    fn test_thresholds() {
        let info = MemoryInfo {
            total_memory: 16_000_000_000,    // 16GB
            available_memory: 1_000_000_000, // 1GB (< 10%, critical)
            used_memory: 15_000_000_000,
            process_memory: 500_000_000,
        };

        assert!(info.is_critical());
        assert!(info.is_low());
    }
}
