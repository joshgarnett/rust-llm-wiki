//! Transfer bounded diagnostics and the exact activation proof across joined jobs.
#[derive(Clone)]
pub(crate) struct WorkerContext {
    activation: Option<std::sync::Arc<crate::storage::maintenance_activation::ActivationProof>>,
    #[cfg(test)]
    query: crate::catalog::query_diagnostics::WorkerContext,
    #[cfg(test)]
    paths: bool,
    #[cfg(test)]
    phases: bool,
}
pub(crate) struct WorkerDelta {
    #[cfg(test)]
    query: crate::catalog::query_diagnostics::WorkerDelta,
    #[cfg(test)]
    paths: Option<crate::vault::paths::profile::PathProfile>,
    #[cfg(test)]
    phases: Option<std::collections::BTreeMap<&'static str, u128>>,
}
impl WorkerContext {
    pub(crate) fn reservation_bytes(&self) -> u64 {
        #[cfg(test)]
        {
            self.query
                .reservation_bytes()
                .max(if self.paths || self.phases {
                    1024 * 1024
                } else {
                    0
                })
        }
        #[cfg(not(test))]
        {
            0
        }
    }
    pub(crate) fn capture() -> Self {
        Self {
            activation: crate::storage::maintenance_activation::current_proof(),
            #[cfg(test)]
            query: crate::catalog::query_diagnostics::WorkerContext::capture(),
            #[cfg(test)]
            paths: crate::vault::paths::profile::active(),
            #[cfg(test)]
            phases: crate::app::refresh_path_profile::worker_active(),
        }
    }
    pub(crate) fn run<T>(self, job: impl FnOnce() -> T) -> (std::thread::Result<T>, WorkerDelta) {
        #[cfg(test)]
        {
            self.query.install();
            if self.paths {
                crate::vault::paths::profile::begin();
            }
            if self.phases {
                crate::app::refresh_path_profile::worker_begin();
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _activation =
                crate::storage::maintenance_activation::enter_worker(self.activation.clone())
                    .expect("worker activation context admission");
            job()
        }));
        let delta = WorkerDelta {
            #[cfg(test)]
            query: self.query.take(),
            #[cfg(test)]
            paths: self.paths.then(crate::vault::paths::profile::finish),
            #[cfg(test)]
            phases: self
                .phases
                .then(crate::app::refresh_path_profile::worker_finish),
        };
        (result, delta)
    }
}
impl WorkerDelta {
    pub(crate) fn merge(self) -> crate::domain::Result<()> {
        #[cfg(test)]
        {
            let result = self.query.merge();
            if let Some(paths) = self.paths {
                crate::vault::paths::profile::merge(paths);
            }
            if let Some(phases) = self.phases {
                crate::app::refresh_path_profile::worker_merge(phases);
            }
            return result;
        }
        #[cfg(not(test))]
        Ok(())
    }
}
