use crate::appservice::{ApplicationService, Config, NoState, Result, State};

pub struct NoConfig;

pub struct ApplicationServiceBuilder<C = NoConfig, S = NoState> {
    config_path: C,
    state: S,
}

impl ApplicationServiceBuilder<NoConfig, NoState> {
    pub fn new() -> Self {
        Self { config_path: NoConfig, state: NoState }
    }
}

impl<C> ApplicationServiceBuilder<C, NoState> {
    pub fn with_state<S>(self, state: S) -> ApplicationServiceBuilder<C, State<S>>
    where
        S: Send + Sync + Clone + 'static,
    {
        ApplicationServiceBuilder { config_path: self.config_path, state: State(state) }
    }
}

impl<S> ApplicationServiceBuilder<NoConfig, S> {
    pub fn configuration_file(self, path: impl Into<String>) -> ApplicationServiceBuilder<String, S> {
        ApplicationServiceBuilder { config_path: path.into(), state: self.state }
    }
}

impl<S> ApplicationServiceBuilder<String, S> {
    fn read_config(&self) -> Result<Config> {
        let file = std::fs::File::open(&self.config_path).map_err(|error| {
            tracing::error!("Unable to open file {}: {}", &self.config_path, error);
            error
        })?;

        let config = serde_yaml::from_reader::<_, Config>(file).map_err(|error| {
            tracing::error!("Unable to parse configuration file: {error}");
            error
        })?;

        Ok(config)
    }
}

impl ApplicationServiceBuilder<String, NoState> {
    pub async fn build(&self) -> Result<ApplicationService<NoState>> {
        let config = self.read_config()?;
        let appservice = ApplicationService::new(config).await?;

        Ok(appservice)
    }
}

impl<S: Send + Sync + Clone + 'static> ApplicationServiceBuilder<String, State<S>> {
    pub async fn build(self) -> Result<ApplicationService<State<S>>> {
        let config = self.read_config()?;
        let appservice = ApplicationService::new_stateful(config, self.state.0).await?;

        Ok(appservice)
    }
}
