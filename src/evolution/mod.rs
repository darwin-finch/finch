// Evolution system for massively parallel self-improvement
// 
// Laptop-scale version: 50-100 forks exploring different optimization vectors
// with memtree coordination and local ONNX model experiments

use crate::memory::memtree::{MemTree, NodeId};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Unique identifier for a fork process
pub type ForkId = u32;

/// Unique identifier for a specific mutation experiment
pub type MutationId = String;

/// Performance metrics for a fork
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub fork_id: ForkId,
    pub mutation_id: MutationId,
    pub start_time: u64,
    pub queries_processed: u32,
    pub successful_responses: u32,
    pub errors: u32,
    pub avg_response_time_ms: f64,
    pub memory_usage_mb: f64,
    pub user_feedback_positive: u32,
    pub user_feedback_negative: u32,
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self {
            fork_id: 0,
            mutation_id: String::new(),
            start_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            queries_processed: 0,
            successful_responses: 0,
            errors: 0,
            avg_response_time_ms: 0.0,
            memory_usage_mb: 0.0,
            user_feedback_positive: 0,
            user_feedback_negative: 0,
        }
    }
}

/// Configuration mutations to explore
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationConfig {
    pub mutation_id: MutationId,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub context_window_usage: Option<f32>, // 0.0 to 1.0
    pub tool_permission_threshold: Option<f32>,
    pub memory_retrieval_k: Option<usize>,
    pub embedding_model: Option<String>,
    pub reasoning_prompt_variant: Option<String>,
}

impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            mutation_id: "baseline".to_string(),
            temperature: Some(0.7),
            top_p: Some(0.9),
            max_tokens: Some(4096),
            context_window_usage: Some(0.8),
            tool_permission_threshold: Some(0.5),
            memory_retrieval_k: Some(5),
            embedding_model: Some("all-MiniLM-L6-v2".to_string()),
            reasoning_prompt_variant: Some("default".to_string()),
        }
    }
}

/// A single fork process in the evolutionary system
#[derive(Debug)]
pub struct ForkProcess {
    pub id: ForkId,
    pub mutation_config: MutationConfig,
    pub process: Option<Child>,
    pub work_dir: PathBuf,
    pub metrics: PerformanceMetrics,
    pub last_heartbeat: SystemTime,
}

impl ForkProcess {
    pub fn new(id: ForkId, mutation_config: MutationConfig) -> Self {
        let work_dir = Path::new("/tmp").join(format!("finch_fork_{}", id));
        let mut metrics = PerformanceMetrics::default();
        metrics.fork_id = id;
        metrics.mutation_id = mutation_config.mutation_id.clone();

        Self {
            id,
            mutation_config,
            process: None,
            work_dir,
            metrics,
            last_heartbeat: SystemTime::now(),
        }
    }

    /// Spawn the fork process with mutation parameters
    pub async fn spawn(&mut self) -> Result<()> {
        // Create work directory
        fs::create_dir_all(&self.work_dir).await?;

        // Write mutation config to file for the fork to read
        let config_path = self.work_dir.join("mutation_config.json");
        let config_json = serde_json::to_string_pretty(&self.mutation_config)?;
        fs::write(config_path, config_json).await?;

        // Spawn finch process with special evolution mode
        let mut cmd = Command::new("./target/release/finch");
        cmd.arg("--evolution-mode")
            .arg("--fork-id")
            .arg(self.id.to_string())
            .arg("--work-dir")
            .arg(&self.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let child = cmd.spawn().context("Failed to spawn fork process")?;
        self.process = Some(child);

        info!("Spawned fork {} with mutation {}", self.id, self.mutation_config.mutation_id);
        Ok(())
    }

    /// Check if the process is still alive
    pub fn is_alive(&mut self) -> bool {
        if let Some(ref mut process) = self.process {
            match process.try_wait() {
                Ok(Some(_)) => false, // Process has exited
                Ok(None) => true,     // Process is still running
                Err(_) => false,      // Error checking process
            }
        } else {
            false
        }
    }

    /// Kill the fork process
    pub fn kill(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.kill().context("Failed to kill fork process")?;
            process.wait().context("Failed to wait for process")?;
        }
        Ok(())
    }

    /// Read performance metrics from the fork's output files
    pub async fn update_metrics(&mut self) -> Result<()> {
        let metrics_path = self.work_dir.join("metrics.json");
        if metrics_path.exists() {
            let metrics_json = fs::read_to_string(metrics_path).await?;
            self.metrics = serde_json::from_str(&metrics_json)?;
        }
        Ok(())
    }
}

/// Coordinator for the evolutionary system
pub struct EvolutionCoordinator {
    pub population_size: usize,
    pub forks: HashMap<ForkId, ForkProcess>,
    pub master_memtree: MemTree,
    pub generation: u32,
    pub mutations_tested: HashMap<MutationId, PerformanceMetrics>,
    pub successful_lineages: Vec<MutationId>,
    pub work_dir: PathBuf,
}

impl EvolutionCoordinator {
    pub fn new(population_size: usize) -> Self {
        let work_dir = Path::new("/tmp").join("finch_evolution");
        Self {
            population_size,
            forks: HashMap::new(),
            master_memtree: MemTree::new(),
            generation: 0,
            mutations_tested: HashMap::new(),
            successful_lineages: Vec::new(),
            work_dir,
        }
    }

    /// Initialize the evolution experiment
    pub async fn initialize(&mut self) -> Result<()> {
        info!("Initializing evolution coordinator with population size {}", self.population_size);
        
        // Create work directory
        fs::create_dir_all(&self.work_dir).await?;

        // Generate initial population with random mutations
        for fork_id in 0..self.population_size as u32 {
            let mutation = self.generate_random_mutation(fork_id);
            let fork = ForkProcess::new(fork_id, mutation);
            self.forks.insert(fork_id, fork);
        }

        Ok(())
    }

    /// Generate a random mutation configuration
    fn generate_random_mutation(&self, fork_id: ForkId) -> MutationConfig {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        MutationConfig {
            mutation_id: format!("gen{}_fork{}", self.generation, fork_id),
            temperature: Some(rng.gen_range(0.1..1.5)),
            top_p: Some(rng.gen_range(0.7..1.0)),
            max_tokens: Some(rng.gen_range(1024..8192)),
            context_window_usage: Some(rng.gen_range(0.5..1.0)),
            tool_permission_threshold: Some(rng.gen_range(0.1..0.9)),
            memory_retrieval_k: Some(rng.gen_range(3..15)),
            embedding_model: Some("all-MiniLM-L6-v2".to_string()), // Keep consistent for now
            reasoning_prompt_variant: Some(["default", "analytical", "creative", "precise"][rng.gen_range(0..4)].to_string()),
        }
    }

    /// Spawn all fork processes
    pub async fn spawn_population(&mut self) -> Result<()> {
        info!("Spawning population of {} forks", self.population_size);

        for (_, fork) in self.forks.iter_mut() {
            if let Err(e) = fork.spawn().await {
                error!("Failed to spawn fork {}: {}", fork.id, e);
            }
            // Stagger spawning to avoid overwhelming the system
            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    /// Monitor the population and collect metrics
    pub async fn monitor_population(&mut self) -> Result<()> {
        info!("Starting population monitoring");

        loop {
            // Check health of all forks
            let mut dead_forks = Vec::new();
            for (fork_id, fork) in self.forks.iter_mut() {
                if !fork.is_alive() {
                    warn!("Fork {} has died", fork_id);
                    dead_forks.push(*fork_id);
                } else {
                    // Update metrics
                    if let Err(e) = fork.update_metrics().await {
                        debug!("Failed to update metrics for fork {}: {}", fork_id, e);
                    }
                }
            }

            // Remove dead forks
            for fork_id in dead_forks {
                if let Some(mut fork) = self.forks.remove(&fork_id) {
                    let _ = fork.kill();
                    // Store final metrics
                    self.mutations_tested.insert(fork.mutation_config.mutation_id.clone(), fork.metrics);
                }
            }

            // Check if we need to evolve to next generation
            if self.should_evolve() {
                self.evolve_generation().await?;
            }

            sleep(Duration::from_secs(5)).await;
        }
    }

    /// Determine if it's time to evolve to the next generation
    fn should_evolve(&self) -> bool {
        // Evolve when most forks have processed at least 10 queries
        let active_forks: Vec<_> = self.forks.values().collect();
        if active_forks.is_empty() {
            return false;
        }

        let ready_forks = active_forks.iter()
            .filter(|fork| fork.metrics.queries_processed >= 10)
            .count();

        ready_forks as f32 / active_forks.len() as f32 > 0.7
    }

    /// Evolve to the next generation
    async fn evolve_generation(&mut self) -> Result<()> {
        info!("Evolving to generation {}", self.generation + 1);

        // Collect final metrics from current generation
        let mut generation_results = Vec::new();
        for fork in self.forks.values() {
            generation_results.push(fork.metrics.clone());
        }

        // Kill all current forks
        for (_, mut fork) in self.forks.drain() {
            let _ = fork.kill();
        }

        // Analyze results and identify successful mutations
        self.analyze_generation_results(generation_results).await?;

        // Generate next generation based on successful patterns
        self.generation += 1;
        for fork_id in 0..self.population_size as u32 {
            let mutation = self.generate_evolved_mutation(fork_id);
            let fork = ForkProcess::new(fork_id, mutation);
            self.forks.insert(fork_id, fork);
        }

        // Spawn new generation
        self.spawn_population().await?;

        Ok(())
    }

    /// Analyze results from the completed generation
    async fn analyze_generation_results(&mut self, results: Vec<PerformanceMetrics>) -> Result<()> {
        info!("Analyzing {} fork results from generation {}", results.len(), self.generation);

        // Calculate fitness score for each result
        let mut scored_results: Vec<_> = results.iter()
            .map(|metrics| {
                let fitness = self.calculate_fitness(metrics);
                (metrics.clone(), fitness)
            })
            .collect();

        // Sort by fitness (higher is better)
        scored_results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Top 25% are considered successful
        let top_performers = scored_results.into_iter()
            .take(results.len() / 4)
            .collect::<Vec<_>>();

        for (metrics, fitness) in &top_performers {
            info!("Successful mutation {}: fitness = {:.3}", metrics.mutation_id, fitness);
            self.successful_lineages.push(metrics.mutation_id.clone());
            
            // Store in memtree with Critical importance
            let success_text = format!(
                "Successful mutation {}: temp={:.2}, top_p={:.2}, fitness={:.3}",
                metrics.mutation_id,
                self.mutations_tested.get(&metrics.mutation_id)
                    .and_then(|m| self.get_mutation_temperature(&m.mutation_id))
                    .unwrap_or(0.0),
                self.mutations_tested.get(&metrics.mutation_id)
                    .and_then(|m| self.get_mutation_top_p(&m.mutation_id))
                    .unwrap_or(0.0),
                fitness
            );
            
            // This would need embeddings - simplified for now
            // self.master_memtree.insert(success_text, embedding, 3)?;
        }

        Ok(())
    }

    /// Calculate fitness score for a set of metrics
    fn calculate_fitness(&self, metrics: &PerformanceMetrics) -> f64 {
        let success_rate = if metrics.queries_processed > 0 {
            metrics.successful_responses as f64 / metrics.queries_processed as f64
        } else {
            0.0
        };

        let error_penalty = metrics.errors as f64 * -0.1;
        
        let feedback_score = if metrics.user_feedback_positive + metrics.user_feedback_negative > 0 {
            metrics.user_feedback_positive as f64 / 
            (metrics.user_feedback_positive + metrics.user_feedback_negative) as f64
        } else {
            0.5 // Neutral if no feedback
        };

        let speed_bonus = if metrics.avg_response_time_ms > 0.0 {
            1.0 / (metrics.avg_response_time_ms / 1000.0).max(0.1) // Faster is better
        } else {
            0.0
        };

        // Weighted combination
        success_rate * 2.0 + feedback_score * 1.5 + speed_bonus * 0.5 + error_penalty
    }

    /// Generate mutation for next generation based on successful patterns
    fn generate_evolved_mutation(&self, fork_id: ForkId) -> MutationConfig {
        // TODO: Implement genetic algorithm based on successful_lineages
        // For now, use random with bias toward successful parameters
        self.generate_random_mutation(fork_id)
    }

    // Helper methods for extracting mutation parameters
    fn get_mutation_temperature(&self, _mutation_id: &str) -> Option<f32> {
        // TODO: Store and retrieve mutation configs
        None
    }

    fn get_mutation_top_p(&self, _mutation_id: &str) -> Option<f32> {
        // TODO: Store and retrieve mutation configs  
        None
    }

    /// Get current evolution statistics
    pub fn get_stats(&self) -> EvolutionStats {
        let alive_count = self.forks.values().count();
        let total_queries: u32 = self.forks.values()
            .map(|f| f.metrics.queries_processed)
            .sum();
        let avg_fitness = if !self.mutations_tested.is_empty() {
            self.mutations_tested.values()
                .map(|m| self.calculate_fitness(m))
                .sum::<f64>() / self.mutations_tested.len() as f64
        } else {
            0.0
        };

        EvolutionStats {
            generation: self.generation,
            population_size: self.population_size,
            alive_forks: alive_count,
            total_queries_processed: total_queries,
            successful_mutations: self.successful_lineages.len(),
            average_fitness: avg_fitness,
        }
    }
}

/// Statistics about the evolution process
#[derive(Debug, Serialize, Deserialize)]
pub struct EvolutionStats {
    pub generation: u32,
    pub population_size: usize,
    pub alive_forks: usize,
    pub total_queries_processed: u32,
    pub successful_mutations: usize,
    pub average_fitness: f64,
}

/// Start the evolution experiment
pub async fn run_evolution_experiment(population_size: usize) -> Result<()> {
    let mut coordinator = EvolutionCoordinator::new(population_size);
    
    coordinator.initialize().await?;
    coordinator.spawn_population().await?;
    coordinator.monitor_population().await?;
    
    Ok(())
}