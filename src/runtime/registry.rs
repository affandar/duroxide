use std::collections::HashMap;
use std::sync::Arc;
use semver::Version;
use crate::{OrchestrationContext};
use crate::_typed_codec::Codec;
use super::OrchestrationHandler;

#[derive(Clone, Default)]
pub struct OrchestrationRegistry {
    pub(crate) inner: Arc<HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>>,
    pub(crate) policy: Arc<tokio::sync::Mutex<HashMap<String, VersionPolicy>>>,
}

#[derive(Clone, Debug)]
pub enum VersionPolicy { Latest, Exact(Version) }

impl OrchestrationRegistry {
    pub fn builder() -> OrchestrationRegistryBuilder {
        OrchestrationRegistryBuilder { map: HashMap::new(), policy: HashMap::new() }
    }

    pub async fn resolve_for_start(&self, name: &str) -> Option<(Version, Arc<dyn OrchestrationHandler>)> {
        let pol = self.policy.lock().await.get(name).cloned().unwrap_or(VersionPolicy::Latest);
        match pol {
            VersionPolicy::Latest => {
                let m = self.inner.get(name)?;
                let (v, h) = m.iter().next_back()?;
                Some((v.clone(), h.clone()))
            }
            VersionPolicy::Exact(v) => {
                let h = self.inner.get(name)?.get(&v)?.clone();
                Some((v, h))
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.iter().next_back().map(|(_v, h)| h.clone())
    }

    pub fn resolve_exact(&self, name: &str, v: &Version) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.get(v).cloned()
    }

    pub async fn set_version_policy(&self, name: &str, policy: VersionPolicy) {
        self.policy.lock().await.insert(name.to_string(), policy);
    }
    pub async fn unpin(&self, name: &str) { self.set_version_policy(name, VersionPolicy::Latest).await; }

    pub fn list_orchestration_names(&self) -> Vec<String> { self.inner.keys().cloned().collect() }
    pub fn list_orchestration_versions(&self, name: &str) -> Vec<Version> {
        self.inner.get(name).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }
}

pub struct OrchestrationRegistryBuilder {
    map: HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>,
    policy: HashMap<String, VersionPolicy>,
}

impl OrchestrationRegistryBuilder {
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        use super::FnOrchestration;
        let name = name.into();
        let v = Version::parse("1.0.0").unwrap();
        let entry = self.map.entry(name.clone()).or_default();
        if entry.contains_key(&v) {
            panic!("duplicate orchestration registration: {}@{} (explicitly register a later version)", name, v);
        }
        entry.insert(v, Arc::new(FnOrchestration(f)));
        self
    }

    pub fn register_typed<In, Out, F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        In: serde::de::DeserializeOwned + Send + 'static,
        Out: serde::Serialize + Send + 'static,
        F: Fn(OrchestrationContext, In) -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Result<Out, String>> + Send + 'static,
    {
        use super::FnOrchestration;
        let f_clone = f.clone();
        let wrapper = move |ctx: OrchestrationContext, input_s: String| {
            let f_inner = f_clone.clone();
            async move {
                let input: In = crate::_typed_codec::Json::decode(&input_s)?;
                let out: Out = f_inner(ctx, input).await?;
                crate::_typed_codec::Json::encode(&out)
            }
        };
        let name = name.into();
        let v = Version::parse("1.0.0").unwrap();
        self.map.entry(name).or_default().insert(v, Arc::new(FnOrchestration(wrapper)));
        self
    }

    pub fn register_versioned<F, Fut>(mut self, name: impl Into<String>, version: impl AsRef<str>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        use super::FnOrchestration;
        let name = name.into();
        let v = Version::parse(version.as_ref()).expect("semver");
        let entry = self.map.entry(name.clone()).or_default();
        if entry.contains_key(&v) {
            panic!("duplicate orchestration registration: {}@{}", name, v);
        }
        if let Some((latest, _)) = entry.iter().next_back() {
            if &v <= latest {
                panic!("non-monotonic orchestration version for {}: {} is not later than existing latest {}", name, v, latest);
            }
        }
        entry.insert(v, Arc::new(FnOrchestration(f)));
        self
    }

    pub fn set_policy(mut self, name: impl Into<String>, policy: VersionPolicy) -> Self {
        self.policy.insert(name.into(), policy);
        self
    }

    pub fn build(self) -> OrchestrationRegistry {
        OrchestrationRegistry { inner: Arc::new(self.map), policy: Arc::new(tokio::sync::Mutex::new(self.policy)) }
    }
}


