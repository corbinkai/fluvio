//!
//! # Clear Topic
//!
//! CLI command to clear all data from a topic by deleting and recreating it.
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

    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    confirm: bool,
}

impl ClearTopicOpt {
    pub async fn process(self, fluvio: &Fluvio) -> Result<()> {
        let admin = fluvio.admin().await;

        // Look up existing topic to get its spec
        let topics = admin.list::<TopicSpec, _>(vec![self.topic.clone()]).await?;
        let topic_meta = topics
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("topic '{}' not found", self.topic))?;

        debug!(topic = %self.topic, "found topic, spec: {:#?}", topic_meta.spec);

        if !self.confirm {
            println!(
                "This will delete ALL data for topic '{}' by deleting and recreating it.",
                self.topic
            );
            println!("Consumer connections will be dropped. Are you sure? [y/N]");

            let mut ans = String::new();
            std::io::stdin().read_line(&mut ans)?;
            let ans = ans.trim_end().to_lowercase();
            if !matches!(ans.as_str(), "y" | "yes") {
                println!("Aborted");
                return Ok(());
            }
        }

        let spec = topic_meta.spec;

        // Delete the topic
        debug!(topic = %self.topic, "deleting topic");
        admin.delete::<TopicSpec>(&self.topic).await?;

        // Recreate with same spec
        debug!(topic = %self.topic, "recreating topic");
        admin.create(self.topic.clone(), false, spec).await?;

        println!("topic \"{}\" cleared", self.topic);
        Ok(())
    }
}
