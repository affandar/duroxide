use super::OrchestrationHandler;
use crate::_typed_codec::Codec;
use crate::OrchestrationContext;
use async_trait::async_trait;
use semver::Version;
use std::collections::HashMap;
use std::sync::Arc;

type HandlerMap = HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>;

#[derive(Clone, Default)]
pub struct OrchestrationRegistry {
    pub(crate) inner: Arc<HandlerMap>,
    pub(crate) policy: Arc<tokio::sync::Mutex<HashMap<String, VersionPolicy>>>,
}

#[derive(Clone, Debug)]
pub enum VersionPolicy {
    Latest,
    Exact(Version),
}

impl OrchestrationRegistry {
    pub fn builder() -> OrchestrationRegistryBuilder {
        OrchestrationRegistryBuilder {
            map: HashMap::new(),
            policy: HashMap::new(),
            errors: Vec::new(),
        }
    }

    /// Create a builder from an existing registry, copying all registered orchestrations.
    ///
    /// This is useful for extending a pre-built registry with additional orchestrations.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let base_registry = some_library::create_orchestration_registry();
    /// let extended = OrchestrationRegistry::builder_from(&base_registry)
    ///     .register("my-custom-orch", my_orchestration)
    ///     .build();
    /// ```
    pub fn builder_from(reg: &OrchestrationRegistry) -> OrchestrationRegistryBuilder {
        let mut map: HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>> =
            HashMap::new();
        for (name, versions) in reg.inner.iter() {
            map.insert(name.clone(), versions.clone());
        }
        OrchestrationRegistryBuilder {
            map,
            policy: HashMap::new(),
            errors: Vec::new(),
        }
    }

    pub async fn resolve_handler(&self, name: &str) -> Option<(Version, Arc<dyn OrchestrationHandler>)> {
        let pol = self
            .policy
            .lock()
            .await
            .get(name)
            .cloned()
            .unwrap_or(VersionPolicy::Latest);
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

    pub async fn resolve_version(&self, name: &str) -> Option<Version> {
        let pol = self
            .policy
            .lock()
            .await
            .get(name)
            .cloned()
            .unwrap_or(VersionPolicy::Latest);
        match pol {
            VersionPolicy::Latest => {
                let m = self.inner.get(name)?;
                let (v, _h) = m.iter().next_back()?;
                Some(v.clone())
            }
            VersionPolicy::Exact(v) => {
                self.inner.get(name)?.get(&v)?;
                Some(v)
            }
        }
    }

    pub fn resolve_handler_exact(&self, name: &str, v: &Version) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.get(v).cloned()
    }

    pub async fn set_version_policy(&self, name: &str, policy: VersionPolicy) {
        self.policy.lock().await.insert(name.to_string(), policy);
    }
    pub async fn unpin(&self, name: &str) {
        self.set_version_policy(name, VersionPolicy::Latest).await;
    }

    pub fn list_orchestration_names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
    pub fn list_orchestration_versions(&self, name: &str) -> Vec<Version> {
        self.inner
            .get(name)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
}

pub struct OrchestrationRegistryBuilder {
    map: HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>,
    policy: HashMap<String, VersionPolicy>,
    errors: Vec<String>,
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
            self.errors
                .push(format!("duplicate orchestration registration: {name}@{v}"));
            return self;
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
        self.map
            .entry(name)
            .or_default()
            .insert(v, Arc::new(FnOrchestration(wrapper)));
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
            self.errors
                .push(format!("duplicate orchestration registration: {name}@{v}"));
            return self;
        }
        if let Some((latest, _)) = entry.iter().next_back()
            && &v <= latest
        {
            panic!("non-monotonic orchestration version for {name}: {v} is not later than existing latest {latest}");
        }
        entry.insert(v, Arc::new(FnOrchestration(f)));
        self
    }

    pub fn register_versioned_typed<In, Out, F, Fut>(
        mut self,
        name: impl Into<String>,
        version: impl AsRef<str>,
        f: F,
    ) -> Self
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
        let v = Version::parse(version.as_ref()).expect("semver");
        let entry = self.map.entry(name.clone()).or_default();
        if entry.contains_key(&v) {
            self.errors
                .push(format!("duplicate orchestration registration: {name}@{v}"));
            return self;
        }
        if let Some((latest, _)) = entry.iter().next_back()
            && &v <= latest
        {
            panic!("non-monotonic orchestration version for {name}: {v} is not later than existing latest {latest}");
        }
        entry.insert(v, Arc::new(FnOrchestration(wrapper)));
        self
    }

    /// Merge another registry into this builder.
    ///
    /// This copies all orchestrations from the other registry into this builder.
    /// Useful for composing registries from multiple library crates.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let combined = OrchestrationRegistry::builder()
    ///     .merge(library1::create_orchestration_registry())
    ///     .merge(library2::create_orchestration_registry())
    ///     .register("my-custom", my_orch)
    ///     .build();
    /// ```
    pub fn merge(mut self, other: OrchestrationRegistry) -> Self {
        for (name, versions) in other.inner.iter() {
            let entry = self.map.entry(name.clone()).or_default();
            for (version, handler) in versions.iter() {
                if entry.contains_key(version) {
                    self.errors
                        .push(format!("duplicate orchestration in merge: {}@{}", name, version));
                } else {
                    entry.insert(version.clone(), handler.clone());
                }
            }
        }
        self
    }

    /// Register multiple orchestrations at once.
    ///
    /// Note: Due to Rust's type system, all functions in the Vec must have identical types.
    /// This is mainly useful with closures or when wrapping functions to match signatures.
    /// For most cases, chaining `.register()` calls is more practical.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::{OrchestrationRegistry, OrchestrationContext};
    /// // Works when all functions have the same type
    /// let handler = |_ctx: OrchestrationContext, input: String| async move {
    ///     Ok(format!("processed: {}", input))
    /// };
    ///
    /// OrchestrationRegistry::builder()
    ///     .register_all(vec![
    ///         ("orch1", handler.clone()),
    ///         ("orch2", handler.clone()),
    ///         ("orch3", handler.clone()),
    ///     ])
    ///     .build();
    ///
    /// // For different function implementations, use chained .register():
    /// // .register("orch1", orch1_fn)
    /// // .register("orch2", orch2_fn)
    /// ```
    pub fn register_all<F, Fut>(mut self, items: Vec<(&str, F)>) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static + Clone,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        for (name, f) in items {
            self = self.register(name, f.clone());
        }
        self
    }

    pub fn set_policy(mut self, name: impl Into<String>, policy: VersionPolicy) -> Self {
        self.policy.insert(name.into(), policy);
        self
    }

    pub fn build(self) -> OrchestrationRegistry {
        OrchestrationRegistry {
            inner: Arc::new(self.map),
            policy: Arc::new(tokio::sync::Mutex::new(self.policy)),
        }
    }

    pub fn build_result(self) -> Result<OrchestrationRegistry, String> {
        if self.errors.is_empty() {
            Ok(OrchestrationRegistry {
                inner: Arc::new(self.map),
                policy: Arc::new(tokio::sync::Mutex::new(self.policy)),
            })
        } else {
            Err(self.errors.join("; "))
        }
    }
}

// ---------------- Activity registry (moved here)

#[async_trait]
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, input: String) -> Result<String, String>;
}

pub struct FnActivity<F, Fut>(pub F)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static;

#[async_trait]
impl<F, Fut> ActivityHandler for FnActivity<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    async fn invoke(&self, input: String) -> Result<String, String> {
        (self.0)(input).await
    }
}

#[derive(Clone, Default)]
pub struct ActivityRegistry {
    pub(crate) inner: Arc<HashMap<String, Arc<dyn ActivityHandler>>>,
}

pub struct ActivityRegistryBuilder {
    map: HashMap<String, Arc<dyn ActivityHandler>>,
}

impl ActivityRegistry {
    pub fn builder() -> ActivityRegistryBuilder {
        // System calls (guid, utcnow_ms, trace) are no longer dispatched as activities.
        // They are handled synchronously during orchestration turns via SystemCall events.
        ActivityRegistryBuilder { map: HashMap::new() }
    }

    /// Get the handler for a specific activity.
    ///
    /// Returns `None` if the activity is not registered.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ActivityHandler>> {
        self.inner.get(name).cloned()
    }

    /// List all registered activity names.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::runtime::registry::ActivityRegistry;
    /// let registry = ActivityRegistry::builder()
    ///     .register("activity1", |_| async { Ok("result".to_string()) })
    ///     .build();
    ///
    /// let names = registry.list_activity_names();
    /// assert!(names.contains(&"activity1".to_string()));
    /// ```
    pub fn list_activity_names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }

    /// Check if an activity is registered.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::runtime::registry::ActivityRegistry;
    /// let registry = ActivityRegistry::builder()
    ///     .register("my-activity", |_| async { Ok("result".to_string()) })
    ///     .build();
    ///
    /// assert!(registry.has("my-activity"));
    /// assert!(!registry.has("unknown-activity"));
    /// ```
    pub fn has(&self, name: &str) -> bool {
        self.inner.contains_key(name)
    }

    /// Get the count of registered activities.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::runtime::registry::ActivityRegistry;
    /// let registry = ActivityRegistry::builder()
    ///     .register("activity1", |_| async { Ok("result".to_string()) })
    ///     .register("activity2", |_| async { Ok("result".to_string()) })
    ///     .build();
    ///
    /// assert_eq!(registry.count(), 2);
    /// ```
    pub fn count(&self) -> usize {
        self.inner.len()
    }
}

impl ActivityRegistryBuilder {
    pub fn from_registry(reg: &ActivityRegistry) -> Self {
        let mut map: HashMap<String, Arc<dyn ActivityHandler>> = HashMap::new();
        for (k, v) in reg.inner.iter() {
            map.insert(k.clone(), v.clone());
        }
        ActivityRegistryBuilder { map }
    }
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnActivity(f)));
        self
    }
    pub fn register_typed<In, Out, F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        In: serde::de::DeserializeOwned + Send + 'static,
        Out: serde::Serialize + Send + 'static,
        F: Fn(In) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Out, String>> + Send + 'static,
    {
        let f_clone = std::sync::Arc::new(f);
        let wrapper = move |input_s: String| {
            let f_inner = f_clone.clone();
            async move {
                let input: In = crate::_typed_codec::Json::decode(&input_s)?;
                let out: Out = (f_inner)(input).await?;
                crate::_typed_codec::Json::encode(&out)
            }
        };
        self.map.insert(name.into(), Arc::new(FnActivity(wrapper)));
        self
    }

    /// Merge another activity registry into this builder.
    ///
    /// This copies all activities from the other registry into this builder.
    /// Useful for composing activity registries from multiple library crates.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let combined = ActivityRegistry::builder()
    ///     .merge(library1::create_activity_registry())
    ///     .merge(library2::create_activity_registry())
    ///     .register("my-custom", my_activity)
    ///     .build();
    /// ```
    pub fn merge(mut self, other: ActivityRegistry) -> Self {
        for (name, handler) in other.inner.iter() {
            if self.map.contains_key(name) {
                // Skip duplicates silently (last registration wins)
                // Alternative: could collect errors like OrchestrationRegistryBuilder
            }
            self.map.insert(name.clone(), handler.clone());
        }
        self
    }

    /// Register multiple activities at once.
    ///
    /// Note: Due to Rust's type system, all functions in the Vec must have identical types.
    /// This is mainly useful with closures or when wrapping functions to match signatures.
    /// For most cases, chaining `.register()` calls is more practical.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::runtime::registry::ActivityRegistry;
    /// // Works when all functions have the same type
    /// let handler = |input: String| async move {
    ///     Ok(format!("processed: {}", input))
    /// };
    ///
    /// ActivityRegistry::builder()
    ///     .register_all(vec![
    ///         ("activity1", handler.clone()),
    ///         ("activity2", handler.clone()),
    ///         ("activity3", handler.clone()),
    ///     ])
    ///     .build();
    ///
    /// // For different function implementations, use chained .register():
    /// // .register("activity1", activity1_fn)
    /// // .register("activity2", activity2_fn)
    /// ```
    pub fn register_all<F, Fut>(mut self, items: Vec<(&str, F)>) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static + Clone,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        for (name, f) in items {
            self = self.register(name, f.clone());
        }
        self
    }

    pub fn build(self) -> ActivityRegistry {
        ActivityRegistry {
            inner: Arc::new(self.map),
        }
    }
}
