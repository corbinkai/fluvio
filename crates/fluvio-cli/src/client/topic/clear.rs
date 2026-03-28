//!
//! # Clear Topic
//!
//! CLI command to clear all data from a topic without deleting it.
//! Preserves topic metadata and consumer connections.
//!

use tracing::debug;
use clap::Parser;
use anyhow::Result;

use fluvio::Fluvio;
use fluvio::metadata::topic::TopicSpec;

#[derive(Debug, Parser)]
pub struct ClearTopicOpt {
    /// Topic name to clear
    #[arg(value_name = "name")]
    topic: String,

    /// Reset high watermark (consumers will re-read from offset 0)
    #[arg(long)]
    reset_hw: bool,

    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    confirm: bool,
}

impl ClearTopicOpt {
    pub async fn process(self, fluvio: &Fluvio) -> Result<()> {
        let admin = fluvio.admin().await;

        if !self.confirm {
            if self.reset_hw {
                println!(
                    "This will clear ALL data for topic '{}' and reset the high watermark.",
                    self.topic
                );
                println!("Consumers will restart from offset 0. Are you sure? [y/N]");
            } else {
                println!(
                    "This will clear ALL data for topic '{}'.",
                    self.topic
                );
                println!("Are you sure? [y/N]");
            }

            let mut ans = String::new();
            std::io::stdin().read_line(&mut ans)?;
            let ans = ans.trim_end().to_lowercase();
            if !matches!(ans.as_str(), "y" | "yes") {
                println!("Aborted");
                return Ok(());
            }
        }

        debug!(topic = %self.topic, reset_hw = %self.reset_hw, "clearing topic");

        if self.reset_hw {
            admin.clear_with_reset_hw::<TopicSpec>(&self.topic).await?;
        } else {
            admin.clear::<TopicSpec>(&self.topic).await?;
        }

        println!("topic \"{}\" cleared", self.topic);
        Ok(())
    }
}
