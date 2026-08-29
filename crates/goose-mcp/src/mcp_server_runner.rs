use std::str::FromStr;

use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};

#[derive(Clone, Debug)]
pub enum McpCommand {
    AutoVisualiser,
    ComputerController,
    EsiDevelopmentVisualizer,
    Memory,
    Tutorial,
}

impl FromStr for McpCommand {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace(' ', "").as_str() {
            "autovisualiser" => Ok(McpCommand::AutoVisualiser),
            "computercontroller" => Ok(McpCommand::ComputerController),
            "esi-development-visualizer" | "esidevelopmentvisualizer" => {
                Ok(McpCommand::EsiDevelopmentVisualizer)
            }
            "memory" => Ok(McpCommand::Memory),
            "tutorial" => Ok(McpCommand::Tutorial),
            _ => Err(format!("Invalid command: {}", s)),
        }
    }
}

impl McpCommand {
    pub fn name(&self) -> &str {
        match self {
            McpCommand::AutoVisualiser => "autovisualiser",
            McpCommand::ComputerController => "computercontroller",
            McpCommand::EsiDevelopmentVisualizer => "esi-development-visualizer",
            McpCommand::Memory => "memory",
            McpCommand::Tutorial => "tutorial",
        }
    }
}

pub async fn serve<S>(server: S) -> Result<()>
where
    S: rmcp::ServerHandler,
{
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("serving error: {:?}", e);
    })?;

    service.waiting().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_esi_development_visualizer_command() {
        let command = McpCommand::from_str("esi-development-visualizer").unwrap();
        assert!(matches!(command, McpCommand::EsiDevelopmentVisualizer));
        assert_eq!(command.name(), "esi-development-visualizer");
    }
}
