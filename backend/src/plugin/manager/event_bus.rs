//! Manager-owned subscriptions for one plugin event bus instance. Callbacks are
//! invoked after the lock is released so they can publish or unsubscribe.
use crate::core::app::error::{InputErrorKind, LimitErrorKind, LookupErrorKind, PluginErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::plugin::types::PluginEventBus;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type Handler = Arc<dyn Fn(Value) + Send + Sync>;
type Subscriptions = HashMap<String, (String, Handler)>;

#[derive(Clone, Default)]
pub struct DefaultPluginEventBus {
    subscriptions: Arc<RwLock<Subscriptions>>,
}

impl DefaultPluginEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    fn validate_event(event_type: &str) -> Result<()> {
        if event_type.is_empty()
            || event_type.len() > 128
            || !event_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(TingError::validation(
                InputErrorKind::InvalidValue,
                "validate_event::validation",
                "Invalid plugin event name".into(),
            ));
        }
        Ok(())
    }
}

impl PluginEventBus for DefaultPluginEventBus {
    fn publish(&self, event_type: &str, data: Value) -> Result<()> {
        Self::validate_event(event_type)?;
        if data.to_string().len() > 1024 * 1024 {
            return Err(TingError::resource_limit(
                LimitErrorKind::Payload,
                "publish::resource_limit",
                "Event exceeds 1 MiB".into(),
            ));
        }
        let handlers: Vec<_> = self
            .subscriptions
            .read()
            .map_err(|_| {
                TingError::plugin_execution(
                    PluginErrorKind::Unavailable,
                    "publish::plugin_execution",
                    "Event subscriptions unavailable".into(),
                )
            })?
            .values()
            .filter(|(name, _)| name == event_type)
            .map(|(_, handler)| Arc::clone(handler))
            .collect();
        for handler in handlers {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data.clone())))
                .map_err(|_| {
                    TingError::plugin_execution(
                        PluginErrorKind::Runtime,
                        "publish::plugin_execution",
                        "Plugin event handler panicked".into(),
                    )
                })?;
        }
        Ok(())
    }
    fn subscribe(
        &self,
        event_type: &str,
        handler: Box<dyn Fn(Value) + Send + Sync>,
    ) -> Result<String> {
        Self::validate_event(event_type)?;
        let mut subscriptions = self.subscriptions.write().map_err(|_| {
            TingError::plugin_execution(
                PluginErrorKind::Unavailable,
                "subscribe::plugin_execution",
                "Event subscriptions unavailable".into(),
            )
        })?;
        if subscriptions.len() >= 1024 {
            return Err(TingError::resource_limit(
                LimitErrorKind::Payload,
                "subscribe::resource_limit",
                "Too many event subscriptions".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        subscriptions.insert(id.clone(), (event_type.into(), Arc::from(handler)));
        Ok(id)
    }
    fn unsubscribe(&self, subscription_id: &str) -> Result<()> {
        let removed = self
            .subscriptions
            .write()
            .map_err(|_| {
                TingError::plugin_execution(
                    PluginErrorKind::Unavailable,
                    "unsubscribe::plugin_execution",
                    "Event subscriptions unavailable".into(),
                )
            })?
            .remove(subscription_id);
        if removed.is_none() {
            return Err(TingError::not_found(
                LookupErrorKind::Resource,
                "unsubscribe::not_found",
                "Event subscription does not exist".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/manager/event_bus.rs"]
mod tests;
