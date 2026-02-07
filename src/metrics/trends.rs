// Training trends tracking for monitoring model improvement over time

use std::collections::VecDeque;

/// Trend direction
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trend {
    Improving,
    Stable,
    Declining,
}

/// Tracks rolling window of training metrics
pub struct TrainingTrends {
    recent_quality_scores: VecDeque<f64>,
    recent_similarities: VecDeque<f64>,
    window_size: usize,
}

impl TrainingTrends {
    /// Create new trends tracker with specified window size
    pub fn new(window_size: usize) -> Self {
        Self {
            recent_quality_scores: VecDeque::with_capacity(window_size),
            recent_similarities: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Add a new measurement to the window
    pub fn add_measurement(&mut self, quality: f64, similarity: Option<f64>) {
        // Add quality score
        if self.recent_quality_scores.len() >= self.window_size {
            self.recent_quality_scores.pop_front();
        }
        self.recent_quality_scores.push_back(quality);

        // Add similarity score if provided
        if let Some(sim) = similarity {
            if self.recent_similarities.len() >= self.window_size {
                self.recent_similarities.pop_front();
            }
            self.recent_similarities.push_back(sim);
        }
    }

    /// Get average quality score over the window
    pub fn avg_quality(&self) -> f64 {
        if self.recent_quality_scores.is_empty() {
            return 0.0;
        }
        self.recent_quality_scores.iter().sum::<f64>() / self.recent_quality_scores.len() as f64
    }

    /// Get average similarity score over the window
    pub fn avg_similarity(&self) -> f64 {
        if self.recent_similarities.is_empty() {
            return 0.0;
        }
        self.recent_similarities.iter().sum::<f64>() / self.recent_similarities.len() as f64
    }

    /// Determine if quality is improving, stable, or declining
    pub fn quality_trend(&self) -> Trend {
        self.calculate_trend(&self.recent_quality_scores)
    }

    /// Determine if similarity is improving, stable, or declining
    pub fn similarity_trend(&self) -> Trend {
        self.calculate_trend(&self.recent_similarities)
    }

    /// Calculate trend by comparing first half vs second half of window
    fn calculate_trend(&self, values: &VecDeque<f64>) -> Trend {
        if values.len() < 4 {
            return Trend::Stable; // Not enough data
        }

        let mid = values.len() / 2;
        let first_half: Vec<f64> = values.iter().take(mid).copied().collect();
        let second_half: Vec<f64> = values.iter().skip(mid).copied().collect();

        let first_avg = first_half.iter().sum::<f64>() / first_half.len() as f64;
        let second_avg = second_half.iter().sum::<f64>() / second_half.len() as f64;

        let diff = second_avg - first_avg;

        // Use threshold to avoid noise
        if diff > 0.05 {
            Trend::Improving
        } else if diff < -0.05 {
            Trend::Declining
        } else {
            Trend::Stable
        }
    }

    /// Get number of measurements recorded
    pub fn measurement_count(&self) -> usize {
        self.recent_quality_scores.len()
    }

    /// Get number of similarity measurements recorded
    pub fn similarity_count(&self) -> usize {
        self.recent_similarities.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_trends() {
        let trends = TrainingTrends::new(10);
        assert_eq!(trends.avg_quality(), 0.0);
        assert_eq!(trends.avg_similarity(), 0.0);
        assert_eq!(trends.quality_trend(), Trend::Stable);
    }

    #[test]
    fn test_add_measurements() {
        let mut trends = TrainingTrends::new(5);
        trends.add_measurement(0.7, Some(0.8));
        trends.add_measurement(0.75, Some(0.85));
        trends.add_measurement(0.8, None);

        assert_eq!(trends.measurement_count(), 3);
        assert_eq!(trends.similarity_count(), 2);
        assert!((trends.avg_quality() - 0.75).abs() < 0.01);
        assert!((trends.avg_similarity() - 0.825).abs() < 0.01);
    }

    #[test]
    fn test_window_overflow() {
        let mut trends = TrainingTrends::new(3);
        trends.add_measurement(0.5, None);
        trends.add_measurement(0.6, None);
        trends.add_measurement(0.7, None);
        trends.add_measurement(0.8, None); // Should evict 0.5

        assert_eq!(trends.measurement_count(), 3);
        assert!((trends.avg_quality() - 0.7).abs() < 0.01); // (0.6 + 0.7 + 0.8) / 3
    }

    #[test]
    fn test_improving_trend() {
        let mut trends = TrainingTrends::new(10);
        // First half: low scores
        trends.add_measurement(0.5, None);
        trends.add_measurement(0.55, None);
        trends.add_measurement(0.6, None);
        trends.add_measurement(0.65, None);
        // Second half: high scores
        trends.add_measurement(0.8, None);
        trends.add_measurement(0.85, None);
        trends.add_measurement(0.9, None);
        trends.add_measurement(0.95, None);

        assert_eq!(trends.quality_trend(), Trend::Improving);
    }

    #[test]
    fn test_declining_trend() {
        let mut trends = TrainingTrends::new(10);
        // First half: high scores
        trends.add_measurement(0.9, None);
        trends.add_measurement(0.85, None);
        trends.add_measurement(0.8, None);
        trends.add_measurement(0.75, None);
        // Second half: low scores
        trends.add_measurement(0.6, None);
        trends.add_measurement(0.55, None);
        trends.add_measurement(0.5, None);
        trends.add_measurement(0.45, None);

        assert_eq!(trends.quality_trend(), Trend::Declining);
    }

    #[test]
    fn test_stable_trend() {
        let mut trends = TrainingTrends::new(10);
        // All scores roughly the same
        for _ in 0..8 {
            trends.add_measurement(0.75, None);
        }

        assert_eq!(trends.quality_trend(), Trend::Stable);
    }
}
