use anyhow::Error;
use ext_php_rs::{convert::{FromZval, IntoZval}, prelude::*, types::{ArrayKey, Zval}};
use futures::future::FutureExt;
use std::collections::HashMap;

/// The Deno main worker. This includes a JsRuntime along with all the standard ops from Deno CLI,
/// such as Deno.core.* and the web APIs such as TextEncoder etc. Use the MainWorker if you want to
/// run programs that are written to run in Deno. The Deno provided ops such as `fetch()` uses it's own
/// TLS and request stack.
#[php_class(name = "Deno\\Runtime\\MainWorker")]
struct MainWorker {
    deno_main_worker: deno_runtime::worker::MainWorker,
    main_module: deno_core::ModuleSpecifier,
}

fn get_error_class_name(e: &deno_core::error::AnyError) -> &'static str {
    deno_runtime::errors::get_error_class_name(e).unwrap_or("Error")
}

#[php_impl(rename_methods = "none")]
impl MainWorker {
    #[constructor]
    fn __construct(
        main_module: &str,
        permissions: &PermissionsOptions,
        options: &WorkerOptions,
    ) -> PhpResult<Self> {
        let main_module = deno_core::resolve_path(main_module).unwrap();
        let permissions =
            match deno_runtime::permissions::Permissions::from_options(&permissions.into()) {
                Ok(p) => p,
                Err(_) => return Err("Unable to parse permissions.".into()),
            };

        let worker = deno_runtime::worker::MainWorker::bootstrap_from_options(
            main_module.clone(),
            permissions,
            options.into(),
        );
        Ok(Self {
            deno_main_worker: worker,
            main_module: main_module,
        })
    }

    pub fn execute_main_module(&mut self) -> PhpResult<()> {
        // todo switch all to use tokio
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&mut rt, async {
            match self
                .deno_main_worker
                .execute_main_module(&self.main_module)
                .await
            {
                Ok(()) => Ok(()),
                Err(error) => return Err(error.to_string().into()),
            }
        })
    }

    fn run_event_loop(&mut self) -> PhpResult<()> {
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&mut rt, async {
            match self.deno_main_worker.run_event_loop(false).await {
                Ok(()) => Ok(()),
                Err(error) => return Err(error.to_string().into()),
            }
        })
    }

    /// Execute JavaSscript inside the V8 Isolate.
    ///
    /// This does not support top level await for Es6 imports. use `load_main_module`
    /// to execute JavaScript in modules.
    fn execute_script(&mut self, name: &str, source_code: &str) -> PhpResult<String> {
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&mut rt, async {
            match self.deno_main_worker.js_runtime.execute_script(name, source_code) {
                Ok(return_value) => {
                    let mut scope = self.deno_main_worker.js_runtime.handle_scope();
                    let value = return_value.open(&mut scope);
                    let value_str = value
                        .to_string(&mut scope)
                        .unwrap()
                        .to_rust_string_lossy(&mut scope);
                    Ok(String::from(value_str))
                },
                Err(error) => match error.downcast::<deno_core::error::JsError>() {
                    Ok(error) => {
                        Err(JsException::from(error).into())
                    },
                    Err(error) => Err(error.to_string().into()),
                },
            }
        })
    }
}

#[php_class(name = "Deno\\Core\\JsException")]
#[extends(ext_php_rs::zend::ce::exception())]
#[derive(Default, Clone)]
pub struct JsException {
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Protected)]
    message: String,
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    code: i32,
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Protected)]
    file: String,
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Protected)]
    line: i64,
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Protected)]
    trace: Vec<String>,
}

impl From<JsException> for PhpException {
    fn from(js_exception: JsException) -> Self {
        use ext_php_rs::class::RegisteredClass;
        let code = js_exception.code.clone();
        let message = js_exception.message.clone();
        let zval = js_exception.into_zval(true).unwrap();
        let mut php_exception = PhpException::new( message, code, JsException::get_metadata().ce() );
        php_exception.set_object(Some(zval.into()));
        php_exception
    }
}

impl From<deno_core::error::JsError> for JsException {
    fn from(error: deno_core::error::JsError) -> Self {
        let source = match error.frames.get(0) {
            Some(frame) => (frame.file_name.clone().unwrap_or("unknown".to_string()),frame.line_number.unwrap_or(0)),
            None => ("unknown".to_string(),0)
        };

        let stack = error.frames.into_iter().map( |frame| {
            format!("{}:{}", frame.file_name.unwrap_or("unknown".to_string()), frame.line_number.unwrap_or(0) )

        } ).collect::<Vec<String>>();

        JsException {
            message: error.message.unwrap_or("Unknown JavaScript error.".to_string()),
            code: 0,
            file: source.0,
            line: source.1,
            trace: stack,
        }
    }
}

#[php_impl]
impl JsException {
    fn __construct() -> Self {
        Self {
            message: "JSError happened".to_owned(),
            code: 0,
            file: "".to_owned(),
            line: 0,
            trace: vec![],
        }
    }
}

/// The options to provide to Deno\Runtime\MainWorker.
#[php_class(name = "Deno\\Runtime\\WorkerOptions")]
#[derive(Debug)]
struct WorkerOptions {
    /// The Deno\Runtime\BootstrapOptions containing options for the bootstrap process.
    ///
    /// @var \Deno\Runtime\BootstrapOptions
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    bootstrap: BootstrapOptions,
    /// Extensions allow you to add additional functionality via Deno "ops" to the JsRuntime. `extensions` takes an array of
    /// Deno\Core\Extension instances. See Deno\Core\Extension for details on the PHP <=> JS functions bridge.
    ///
    /// @var Deno\Core\Extension[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    extensions: Vec<Extension>,
    /// The module loader accepts a callable which is responsible for loading
    /// ES6 modules from a given name. See `Deno\Core\ModuleLoader` for methods that should be implemented.
    ///
    /// @var Deno\Core\ModuleLoader
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    module_loader: CloneableZval,
}

#[php_impl(rename_methods = "none")]
impl WorkerOptions {
    fn __construct(
        bootstrap: &BootstrapOptions,
        extensions: Vec<Extension>,
        module_loader: CloneableZval,
    ) -> Self {
        Self {
            bootstrap: bootstrap.clone(),
            extensions,
            module_loader,
        }
    }
}

impl From<&WorkerOptions> for deno_runtime::worker::WorkerOptions {
    fn from(options: &WorkerOptions) -> Self {
        let create_web_worker_cb = std::sync::Arc::new(|_| {
            todo!("Web workers are not supported in the example");
        });
        let web_worker_event_cb = std::sync::Arc::new(|_| {
            todo!("Web workers are not supported in the example");
        });

        let module_loader: CloneableZval = options.module_loader.clone();

        deno_runtime::worker::WorkerOptions {
            bootstrap: (&options.bootstrap).try_into().unwrap(),
            extensions: options.extensions.iter().map(|e| e.into()).collect(),
            unsafely_ignore_certificate_errors: None,
            root_cert_store: None,
            seed: None,
            source_map_getter: None,
            format_js_error_fn: None,
            web_worker_preload_module_cb: web_worker_event_cb.clone(),
            web_worker_pre_execute_module_cb: web_worker_event_cb,
            create_web_worker_cb,
            maybe_inspector_server: None,
            should_break_on_first_statement: false,
            module_loader: std::rc::Rc::new(ModuleLoader::new(module_loader)),
            npm_resolver: None,
            get_error_class_fn: Some(&get_error_class_name),
            origin_storage_dir: None,
            blob_store: deno_runtime::deno_web::BlobStore::default(),
            broadcast_channel: deno_broadcast_channel::InMemoryBroadcastChannel::default(),
            shared_array_buffer_store: None,
            compiled_wasm_module_store: None,
            stdio: Default::default(),
        }
    }
}

/// Common bootstrap options for MainWorker & WebWorker
#[derive(Clone, Debug)]
#[php_class(name = "Deno\\Runtime\\BootstrapOptions")]
struct BootstrapOptions {
    /// Sets `Deno.args` in JS runtime.
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    args: Vec<String>,
    /// @var int
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    cpu_count: usize,
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    debug_flag: bool,
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    enable_testing_features: bool,
    /// @var ?string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    location: Option<String>,
    /// Sets `Deno.noColor` in JS runtime.
    ///
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    no_color: bool,
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    is_tty: bool,
    /// Sets `Deno.version.deno` in JS runtime.
    ///
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    runtime_version: String,
    /// Sets `Deno.version.typescript` in JS runtime.
    ///
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    ts_version: String,
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    unstable: bool,
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    user_agent: String,
}

#[php_impl(rename_methods = "none")]
impl BootstrapOptions {
    fn __construct() -> Self {
        BootstrapOptions {
            args: vec![],
            cpu_count: 1,
            debug_flag: false,
            enable_testing_features: false,
            location: None,
            no_color: false,
            is_tty: false,
            runtime_version: "x".to_string(),
            ts_version: "x".to_string(),
            unstable: false,
            user_agent: "hello_runtime".to_string(),
        }
    }
}
impl TryFrom<&BootstrapOptions> for deno_runtime::BootstrapOptions {
    type Error = String;
    fn try_from(options: &BootstrapOptions) -> Result<deno_runtime::BootstrapOptions, String> {
        Ok(deno_runtime::BootstrapOptions {
            args: options.args.clone(),
            cpu_count: options.cpu_count,
            debug_flag: options.debug_flag,
            enable_testing_features: options.enable_testing_features,
            location: match options.location.clone() {
                Some(location) => url::Url::parse(location.as_str()).ok(),
                None => None,
            },
            no_color: options.no_color,
            is_tty: options.is_tty,
            runtime_version: options.runtime_version.clone(),
            ts_version: options.ts_version.clone(),
            unstable: options.unstable,
            user_agent: options.user_agent.clone(),
        })
    }
}

#[php_class(name = "Deno\\Runtime\\PermissionsOptions")]
struct PermissionsOptions {
    /// Allow environment access for things like getting and setting of environment variables. You can specify a list of environment variables to provide an allow-list of allowed environment variables. Pass an empty array to allow all.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_env: Option<Vec<String>>,
    /// Allow high-resolution time measurement. High-resolution time can be used in timing attacks and fingerprinting.
    ///
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_hrtime: bool,
    /// Allow network access. You can specify an optional list of IP addresses or hostnames (optionally with ports) to provide an allow-list of allowed network addresses. Pass an empty array to allow all.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_net: Option<Vec<String>>,
    /// Allow loading of dynamic libraries. Be aware that dynamic libraries are not run in a sandbox and therefore do not have the same security restrictions as the Deno process. Therefore, use with caution.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_ffi: Option<Vec<String>>,
    /// Allow file system read access. You can specify an optional list of directories or files to provide an allow-list of allowed file system access. Pass an empty array to allow all.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_read: Option<Vec<String>>,
    /// Allow running subprocesses. You can specify an optional list of subprocesses to provide an allow-list of allowed subprocesses.
    /// Be aware that subprocesses are not run in a sandbox and therefore do not have the same security restrictions as the Deno process. Therefore, use with caution. Pass an empty array to allow all.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_run: Option<Vec<String>>,
    /// Allow file system write access. You can specify an optional list of directories or files to provide an allow-list of allowed file system access. Pass an empty array to allow all.
    ///
    /// @var string[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    allow_write: Option<Vec<String>>,
}

#[php_impl(rename_methods = "none")]
impl PermissionsOptions {
    fn __construct() -> Self {
        Self {
            allow_env: None,
            allow_hrtime: false,
            allow_net: None,
            allow_ffi: None,
            allow_read: None,
            allow_run: None,
            allow_write: None,
        }
    }
}

impl From<&PermissionsOptions> for deno_runtime::permissions::PermissionsOptions {
    fn from(options: &PermissionsOptions) -> Self {
        deno_runtime::permissions::PermissionsOptions {
            allow_env: options.allow_env.clone(),
            allow_hrtime: options.allow_hrtime,
            allow_net: options.allow_net.clone(),
            allow_ffi: options
                .allow_ffi
                .clone()
                .map(|vec| vec.iter().map(|a| std::path::PathBuf::from(a)).collect()),
            allow_read: options
                .allow_read
                .clone()
                .map(|vec| vec.iter().map(|a| std::path::PathBuf::from(a)).collect()),
            allow_run: options.allow_run.clone(),
            allow_write: options
                .allow_write
                .clone()
                .map(|vec| vec.iter().map(|a| std::path::PathBuf::from(a)).collect()),
            prompt: false,
        }
    }
}

/// The options provided to the JsRuntime. Pass an instance of this class
/// to Deno\Core\JsRuntime.
///
#[php_class(name = "Deno\\Core\\RuntimeOptions")]
#[derive(Debug)]
struct RuntimeOptions {
    /// The module loader accepts a callable which is responsible for loading
    /// ES6 modules from a given name. See `Deno\Core\ModuleLoader` for methods that should be implemented.
    /// @var Deno\Core\ModuleLoader
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    module_loader: Option<CloneableZval>,
    /// Extensions allow you to add additional functionality via Deno "ops" to the JsRuntime. `extensions` takes an array of
    /// Deno\Core\Extension instances. See Deno\Core\Extension for details on the PHP <=> JS functions bridge.
    /// @var Deno\Core\Extension[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    extensions: Vec<Extension>,
    /// Prepare runtime to take snapshot of loaded code. The snapshot is determinstic and uses predictable random numbers.
    ///
    /// Currently can’t be used with startup_snapshot.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    will_snapshot: bool,
    /// V8 snapshot that should be loaded on startup.
    ///
    /// Currently can’t be used with will_snapshot.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    startup_snapshot: Option<CloneableZval>,
}

#[php_impl(rename_methods = "none")]
impl RuntimeOptions {
    #[constructor]
    fn __construct() -> Self {
        Self {
            module_loader: None,
            extensions: vec![],
            will_snapshot: false,
            startup_snapshot: None,
        }
    }
}

impl From<&RuntimeOptions> for deno_core::RuntimeOptions {
    fn from(options: &RuntimeOptions) -> Self {
        let extensions: Vec<deno_core::Extension> = options
            .extensions
            .iter()
            .map(|extension| extension.into())
            .collect();

        let module_loader: Option<CloneableZval> = match options.module_loader.as_ref() {
            Some(module_loader) => Some(module_loader.clone()),
            None => None,
        };

        deno_core::RuntimeOptions {
            module_loader: match module_loader {
                Some(module_loader) => Some(std::rc::Rc::new(ModuleLoader::new(module_loader))),
                None => None,
            },
            extensions,
            will_snapshot: options.will_snapshot,
            startup_snapshot: match &options.startup_snapshot {
                Some(snapshot) => {
                    let snapshot = snapshot.clone().into_zval(false).unwrap().binary().unwrap();
                    Some(deno_core::Snapshot::Boxed(
                        snapshot.as_slice().to_vec().into_boxed_slice(),
                    ))
                }
                None => None,
            },
            ..Default::default()
        }
    }
}

#[php_class(name = "Deno\\Core\\JsRuntime")]
/// The JsRuntime is a wrapper around a V8 isolate. It can execute ES6 including ES6 modules. The JsRuntime
/// does not include any of the Deno.core.* ops, and does not provide implementations for web apis, such as
/// fetch(). Use JsRuntime if you want to provide low-level v8 isolates, and implement extensions for all
/// functionality such as local storage, remote requests etc.
struct JsRuntime {
    deno_jsruntime: deno_core::JsRuntime,
    will_snapshot: bool,
    has_snapshotted: bool,
}

#[php_impl(rename_methods = "none")]
impl JsRuntime {
    #[constructor]
    fn __construct(options: &RuntimeOptions) -> Self {
        let mut deno_jsruntime = deno_core::JsRuntime::new(options.into());
        let mut callbacks: HashMap<String, CloneableZval> = HashMap::new();

        for extension in &options.extensions {
            for (name, op) in &extension.ops {
                callbacks.insert(name.to_string(), op.clone().into());
            }
        }

        deno_jsruntime
            .v8_isolate()
            .set_slot(std::rc::Rc::new(std::cell::RefCell::new(callbacks)));

        Self {
            deno_jsruntime: deno_jsruntime,
            will_snapshot: options.will_snapshot,
            has_snapshotted: false,
        }
    }

    /// Execute JavaSscript inside the V8 Isolate.
    ///
    /// This does not support top level await for Es6 imports. use `load_main_module`
    /// to execute JavaScript in modules.
    fn execute_script(&mut self, name: &str, source_code: &str) -> PhpResult<String> {
        if self.has_snapshotted {
            return Err("Scripts can not be executed after JsRuntime has been snapshotted.".into());
        }
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&mut rt, async {
            match self.deno_jsruntime.execute_script(name, source_code) {
                Ok(return_value) => {
                    let mut scope = self.deno_jsruntime.handle_scope();
                    let value = return_value.open(&mut scope);
                    let value_str = value
                        .to_string(&mut scope)
                        .unwrap()
                        .to_rust_string_lossy(&mut scope);
                    Ok(String::from(value_str))
                },
                Err(error) => match error.downcast::<deno_core::error::JsError>() {
                    Ok(error) => {
                        Err(JsException::from(error).into())
                    },
                    Err(error) => Err(error.to_string().into()),
                },
            }
        })
    }

    /// Load an ES6 module as the main starting module.
    ///
    /// This function returns a module ID which should be passed to `mod_evaluate()`.
    ///
    /// @return int
    fn load_main_module(
        &mut self,
        specifier: &str,
        code: Option<String>,
    ) -> PhpResult<deno_core::ModuleId> {
        let specifier = match url::Url::parse(specifier) {
            Ok(specifier) => specifier,
            Err(err) => return Err(err.to_string().into()),
        };

        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&mut rt, async {
            match self.deno_jsruntime.load_main_module(&specifier, code).await {
                Ok(module_id) => Ok(module_id),
                Err(error) => return Err(error.to_string().into()),
            }
        })
    }

    /// Evaluate a given module ID. This will run all schyonous code in the module.
    /// If there are pending Promises or async axtions, use `run_event_loop()` to
    /// wait until all async actions complete.
    fn mod_evaluate(&mut self, id: deno_core::ModuleId) -> PhpResult<()> {
        let result = self.deno_jsruntime.mod_evaluate(id);
        match futures::executor::block_on(self.deno_jsruntime.run_event_loop(false)) {
            Ok(()) => (),
            Err(error) => return Err(error.to_string().into()),
        };

        match futures::executor::block_on(result).unwrap() {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_string().into()),
        }
    }

    /// Wait for the event loop to run all pending async actions.
    fn run_event_loop(&mut self) -> PhpResult<()> {
        match futures::executor::block_on(self.deno_jsruntime.run_event_loop(false)) {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_string().into()),
        }
    }

    /// Takes a snapshot. The isolate should have been created with will_snapshot set to true.
    ///
    /// @return string
    fn snapshot(&mut self) -> PhpResult<Zval> {
        if self.will_snapshot == false {
            return Err(
                "Unable to shapshot JsRuntime when RuntimeOptions.will_snapshot is not true."
                    .into(),
            );
        }
        let startup_data = self.deno_jsruntime.snapshot();
        let snapshot_slice: &[u8] = &*startup_data;
        let mut zval = Zval::new();
        zval.set_binary(snapshot_slice.to_vec());
        self.has_snapshotted = true;
        Ok(zval)
    }
}
/// The module loader interface (don't trust the docs, this is an interface not a class!)
/// Pass an instance of your class that implements `Deno\Core\ModuleLoader` to the `module_loader`
/// property of `Deno\Runtime\WorkerOptions` or `Deno\Core\RuntimeOptions`
#[php_class(name = "Deno\\Core\\ModuleLoader", flags = "Interface")]
#[derive(Clone, Debug)]
struct ModuleLoaderInterface {}

#[php_impl(rename_methods = "none")]
impl ModuleLoaderInterface {
    /// The `resolve` method should take a module specifier and normalize it to a canonical URL.
    /// @return string
    #[php_method]
    #[abstract_method]
    fn resolve(&self, _specifier: &str, _referrer: &str) -> &str {
        ""
    }

    /// The `load` method takes a module specifier and should return the contents for a module.
    /// See `Deno\Core\ModuleSource` for the specifics.
    /// @return \Deno\Core\ModuleSource
    #[php_method]
    #[abstract_method]
    fn load(&self, _specifier: &str) -> Option<ModuleSource> {
        None
    }
}

#[derive(Clone)]
struct ModuleLoader(CloneableZval);

impl ModuleLoader {
    fn new(loader: CloneableZval) -> Self {
        Self(loader)
    }
}

impl deno_core::ModuleLoader for ModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _is_main: bool,
    ) -> Result<deno_core::ModuleSpecifier, Error> {
        let result = call_user_method!(
            (&self.0).clone().into_zval(false).unwrap(),
            "resolve",
            specifier,
            referrer,
            _is_main
        );

        match result {
            Some(result) => match result.string() {
                Some(result) => match url::Url::parse(result.as_str()) {
                    Ok(result) => Ok(result),
                    Err(err) => anyhow::bail!(err.to_string()),
                },
                None => anyhow::bail!("resolve() did not return a valid string."),
            },
            None => {
                anyhow::bail!("resolve() did not return a valid string.")
            }
        }
    }

    fn load(
        &self,
        _module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<deno_core::ModuleSpecifier>,
        _is_dyn_import: bool,
    ) -> core::pin::Pin<Box<deno_core::ModuleSourceFuture>> {
        let result = call_user_method!(
            (&self.0).clone().into_zval(false).unwrap(),
            "load",
            _module_specifier.to_string().clone()
        );

        let result = match result {
            Some(result) => result,
            None => {
                return async {
                    Err(deno_core::error::generic_error(
                        "Error calling load() function on ModuleLoader",
                    ))
                }
                .boxed_local()
            }
        };

        let source: &ModuleSource = match result.extract() {
            Some(source) => source,
            None => {
                return async {
                    Err(deno_core::error::generic_error(
                        "Error converting return value of load() to ModuleSource",
                    ))
                }
                .boxed_local()
            }
        };

        let module_source = deno_core::ModuleSource {
            code: source.code.clone().as_bytes().to_owned().into_boxed_slice(),
            module_type: if source.module_type == "json" {
                deno_core::ModuleType::Json
            } else {
                deno_core::ModuleType::JavaScript
            },
            module_url_specified: source.module_url_specified.clone(),
            module_url_found: source.module_url_found.clone(),
        };

        return async { Ok(module_source) }.boxed_local();
    }
}

/// Attempts to call a given PHP callable.
///
/// # Parameters
///
/// * `$fn` - The 'function' to call. Can be an [`Arg`] or a [`Zval`].
/// * ...`$param` - The parameters to pass to the function. Must be able to be
///   converted into a [`Zval`].
///
/// [`Arg`]: crate::args::Arg
/// [`Zval`]: crate::types::Zval
#[macro_export]
macro_rules! call_user_method {
    ($class: expr, $method: expr, $($param: expr),*) => {
        {
        let mut hashtable = ext_php_rs::types::ZendHashTable::new();
        hashtable.insert_at_index(0, $class).ok();
        hashtable.insert_at_index(1, $method).ok();

        let result = hashtable.into_zval(false).unwrap().try_call(vec![$(&$param),*]);

        // let result = ext_php_rs::call_user_func!(
        //     hashtable.into_zval(false).unwrap(),
        //     $(&$param),*
        // );

        result.ok()
    }
    };
}

/// JsFile is a descriptor for JavaScript files that are loaded as
/// part of the Extension->js_files array. The `code` of `JsFile` is
/// executed when the JsRuntime is initiated.
#[php_class(name = "Deno\\Core\\JsFile")]
#[derive(Clone, Debug)]
struct JsFile {
    /// The filename for the JS file
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    filename: String,
    /// The code for the javascript file
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    code: String,
}

#[php_impl(rename_methods = "none")]
impl JsFile {
    #[constructor]
    fn __construct(filename: String, code: String) -> Self {
        Self { filename, code }
    }
}

/// Extension contains PHP functions (ops) and associated js files which are
/// exposed to JavaScript via the JsRuntime. PHP functions can be called from JavaScript
/// via `Deno.core.$name` where `$name` is the array key string from the `ops` property.
///
/// It's common to provide `ops` and also more user-friendly accessible functions for those
/// `ops` via the `js_files` property.
#[php_class(name = "Deno\\Core\\Extension")]
#[derive(Clone, Debug)]
struct Extension {
    /// The JS files that should be loaded into the V8 Isolate.
    /// @var Deno\Core\JsFile[]
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    js_files: Vec<JsFile>,
    /// The ops for the extension (bridged to PHP functions)
    /// @var array<string, callable>
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    ops: HashMap<String, CloneableZval>,
}

#[php_impl(rename_methods = "none")]
impl Extension {
    #[constructor]
    fn __construct() -> Self {
        Self {
            js_files: vec![],
            ops: HashMap::new(),
        }
    }
}

impl From<Extension> for deno_core::Extension {
    fn from(extension: Extension) -> Self {
        use deno_core::v8::MapFnTo;
        let js_files = extension
            .js_files
            .iter()
            .map(|js_file| -> (&str, &str) {
                // This causes a memory leak, but the js-files exntesion requires static strings so there's not much we can do.
                let filename: &'static str = Box::leak(js_file.filename.clone().into_boxed_str());
                let code: &'static str = Box::leak(js_file.code.clone().into_boxed_str());
                (filename, code)
            })
            .collect();
        let mut ops: Vec<deno_core::OpDecl> = vec![];
        for (name, _op) in &extension.ops {
            let static_name: &'static str = Box::leak(name.clone().into_boxed_str());
            let op_decl = deno_core::OpDecl {
                name: static_name,
                v8_fn_ptr: op_callback.map_fn_to(),
                enabled: true,
                fast_fn: None,
                is_async: false,
                is_unstable: false,
                is_v8: false,
            };

            ops.push(op_decl);
        }
        deno_core::Extension::builder()
            .js(js_files)
            .ops(ops)
            .build()
    }
}

impl From<&Extension> for deno_core::Extension {
    fn from(extension: &Extension) -> Self {
        extension.clone().into()
    }
}

impl FromZval<'_> for Extension {
    const TYPE: ext_php_rs::flags::DataType = ext_php_rs::flags::DataType::Mixed;
    fn from_zval(zval: &'_ Zval) -> Option<Self> {
        let extension: &Extension = zval.extract().unwrap();
        let new_extension = extension.to_owned();
        Some(new_extension)
    }
}

impl FromZval<'_> for BootstrapOptions {
    const TYPE: ext_php_rs::flags::DataType = ext_php_rs::flags::DataType::Mixed;
    fn from_zval(zval: &'_ Zval) -> Option<Self> {
        let bootstrap: &BootstrapOptions = zval.extract().unwrap();
        let new_bootstrap = bootstrap.to_owned();
        Some(new_bootstrap)
    }
}

impl FromZval<'_> for JsFile {
    const TYPE: ext_php_rs::flags::DataType = ext_php_rs::flags::DataType::Mixed;
    fn from_zval(zval: &'_ Zval) -> Option<Self> {
        let file: &JsFile = zval.extract().unwrap();
        let new_file = file.to_owned();
        Some(new_file)
    }
}

/// ModuleSource represents an ES6 module, including the source code and type. An ModuleSource should
/// be returned from your module loader passed to JsRuntime's RuntimeOptions::module_loader property.
#[php_class(name = "Deno\\Core\\ModuleSource")]
#[derive(Debug)]
struct ModuleSource {
    /// The module's source code.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    code: String,
    /// The module type, can be "javascript" or "json".
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    module_type: String,
    /// The specified module URL of the import.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    module_url_specified: String,
    /// The resolved module URL, after things like 301 redrects etc.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    module_url_found: String,
}

#[php_impl(rename_methods = "none")]
impl ModuleSource {
    #[constructor]
    fn __construct(
        code: String,
        module_type: String,
        module_url_specified: String,
        module_url_found: String,
    ) -> Self {
        Self {
            code,
            module_type,
            module_url_specified,
            module_url_found,
        }
    }
}

/// ParseParams represent the arguments for Deno\AST\parse_module, which is used to
/// parse TypeScript.
#[php_class(name = "Deno\\AST\\ParseParams")]
struct ParseParams {
    /// The ES6 module specifier, must be a URL.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    specifier: String,
    /// The source code of the ES6 module.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    text_info: String,
    /// The type of the module, specified as a mime-type such as application/typescript etc.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    media_type: String,
}

#[php_impl(rename_methods = "none")]
impl ParseParams {
    fn __construct() -> PhpResult<Self> {
        Ok(Self {
            specifier: "".to_string(),
            media_type: "javascript".to_string(),
            text_info: "".to_string(),
        })
    }
}

impl TryFrom<&ParseParams> for deno_ast::ParseParams {
    type Error = String;
    fn try_from(params: &ParseParams) -> Result<Self, String> {
        let media_type = match url::Url::parse(params.specifier.as_str()) {
            Ok(t) => t,
            Err(err) => return Err(err.to_string()),
        };

        Ok(deno_ast::ParseParams {
            specifier: params.specifier.clone(),
            text_info: deno_ast::SourceTextInfo::from_string(params.text_info.clone()),
            capture_tokens: false,
            maybe_syntax: None,
            scope_analysis: false,
            media_type: deno_ast::MediaType::from_content_type(
                &media_type,
                params.media_type.clone(),
            ),
        })
    }
}

/// The transpiled code to TypeScript source code, this is the result of `Deno\AST\ParsedSource::transpile().
#[php_class(name = "Deno\\AST\\TranspiledSource")]
struct TranspiledSource {
    /// Transpiled text.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub text: String,
    /// Source map back to the original file.
    /// @var string|null
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub source_map: Option<String>,
}

#[php_class(name = "Deno\\AST\\ParsedSource")]
struct ParsedSource {
    deno_ast_parsed_source: deno_ast::ParsedSource,
}

/// Represents the parsed AST via `Deno\AST\parse_modue()`.
#[php_impl(rename_methods = "none")]
impl ParsedSource {
    /// Transpile the ASP to TypeScript, with the provided EmitOptions. Throws an exception or returns Deno\AST\TranspiledSource
    fn transpile(&self, options: &EmitOptions) -> PhpResult<TranspiledSource> {
        match self.deno_ast_parsed_source.transpile(&options.into()) {
            Ok(transpiled_source) => Ok(TranspiledSource {
                text: transpiled_source.text,
                source_map: transpiled_source.source_map,
            }),
            Err(error) => Err(error.to_string().into()),
        }
    }
}

/// TypeScript compiler options used when transpiling.
#[php_class(name = "Deno\\AST\\EmitOptions")]
struct EmitOptions {
    /// When emitting a legacy decorator, also emit experimental decorator meta
    /// data.  Defaults to `false`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub emit_metadata: bool,
    /// Should the source map be inlined in the emitted code file, or provided
    /// as a separate file.  Defaults to `true`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub inline_source_map: bool,
    /// Should the sources be inlined in the source map.  Defaults to `true`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub inline_sources: bool,
    /// `true` if the program should use an implicit JSX import source/the "new"
    /// JSX transforms.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub jsx_automatic: bool,
    /// If JSX is automatic, if it is in development mode, meaning that it should
    /// import `jsx-dev-runtime` and transform JSX using `jsxDEV` import from the
    /// JSX import source as well as provide additional debug information to the
    /// JSX factory.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub jsx_development: bool,
    /// When transforming JSX, what value should be used for the JSX factory.
    /// Defaults to `React.createElement`.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub jsx_factory: String,
    /// When transforming JSX, what value should be used for the JSX fragment
    /// factory.  Defaults to `React.Fragment`.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub jsx_fragment_factory: String,
    /// The string module specifier to implicitly import JSX factories from when
    /// transpiling JSX.
    /// @var string
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub jsx_import_source: Option<String>,
    /// Should a corresponding .map file be created for the output. This should be
    /// false if inline_source_map is true. Defaults to `false`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub source_map: bool,
    /// Should JSX be transformed or preserved.  Defaults to `true`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub transform_jsx: bool,
    /// Should import declarations be transformed to variable declarations using
    /// a dynamic import. This is useful for import & export declaration support
    /// in script contexts such as the Deno REPL.  Defaults to `false`.
    /// @var bool
    #[prop(flags = ext_php_rs::flags::PropertyFlags::Public)]
    pub var_decl_imports: bool,
}

#[php_impl(rename_methods = "none")]
impl EmitOptions {
    fn __construct() -> EmitOptions {
        return EmitOptions {
            emit_metadata: false,
            inline_source_map: true,
            inline_sources: true,
            source_map: false,
            jsx_automatic: false,
            jsx_development: false,
            jsx_factory: "React.createElement".into(),
            jsx_fragment_factory: "React.Fragment".into(),
            jsx_import_source: None,
            transform_jsx: true,
            var_decl_imports: false,
        };
    }
}

impl From<&EmitOptions> for deno_ast::EmitOptions {
    fn from(options: &EmitOptions) -> deno_ast::EmitOptions {
        deno_ast::EmitOptions {
            emit_metadata: options.emit_metadata,
            imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
            inline_source_map: options.inline_source_map,
            inline_sources: options.inline_sources,
            jsx_automatic: options.jsx_automatic,
            jsx_development: options.jsx_development,
            jsx_factory: options.jsx_factory.clone(),
            jsx_fragment_factory: options.jsx_fragment_factory.clone(),
            jsx_import_source: options.jsx_import_source.clone(),
            source_map: options.source_map,
            transform_jsx: options.transform_jsx,
            var_decl_imports: options.var_decl_imports,
        }
    }
}

/// Parse a TypeScript (or similar) module. See ParseParams for options.
#[php_function(ignore_module, name = "Deno\\AST\\parse_module")]
fn parse_module(params: &ParseParams) -> PhpResult<ParsedSource> {
    match deno_ast::parse_module(params.try_into()?) {
        Ok(parsed_source) => Ok(ParsedSource {
            deno_ast_parsed_source: parsed_source,
        }),
        Err(diagnostic) => Err(diagnostic.to_string().into()),
    }
}

// Zval doesn't implement Clone, which means that Zval's can not
// be passed to `ZendCallable.try_call()`, so we have to wrap it
// in a Cloneable wrapper.
#[derive(Debug)]
struct CloneableZval(Zval);

impl FromZval<'_> for CloneableZval {
    const TYPE: ext_php_rs::flags::DataType = ext_php_rs::flags::DataType::Mixed;
    fn from_zval(zval: &'_ Zval) -> Option<Self> {
        Some(Self(zval.shallow_clone()))
    }
}

impl IntoZval for CloneableZval {
    const TYPE: ext_php_rs::flags::DataType = ext_php_rs::flags::DataType::Mixed;
    fn set_zval(self, zv: &mut Zval, _: bool) -> ext_php_rs::error::Result<()> {
        *zv = self.0;
        Ok(())
    }
    fn into_zval(self, _persistent: bool) -> ext_php_rs::error::Result<Zval> {
        Ok(self.0)
    }
}

impl Clone for CloneableZval {
    fn clone(&self) -> Self {
        Self(self.0.shallow_clone())
    }
}

pub fn zval_from_jsvalue(result: v8::Local<v8::Value>, scope: &mut v8::HandleScope) -> Zval {
    if result.is_string() {
        return result.to_rust_string_lossy(scope).try_into().unwrap();
    }
    if result.is_null_or_undefined() {
        let mut zval = Zval::new();
        zval.set_null();
        return zval;
    }
    if result.is_boolean() {
        return result.boolean_value(scope).into();
    }
    if result.is_int32() {
        return result.integer_value(scope).unwrap().try_into().unwrap();
    }
    if result.is_number() {
        return result.number_value(scope).unwrap().into();
    }
    if result.is_array() {
        let array = v8::Local::<v8::Array>::try_from(result).unwrap();
        let mut zend_array = ext_php_rs::types::ZendHashTable::new();
        for index in 0..array.length() {
            let _result = zend_array.push(zval_from_jsvalue(
                array.get_index(scope, index).unwrap(),
                scope,
            ));
        }
        let mut zval = Zval::new();
        zval.set_hashtable(zend_array);
        return zval;
    }
    if result.is_function() {
        return "Function".try_into().unwrap();
    }
    if result.is_object() {
        let object = v8::Local::<v8::Object>::try_from(result).unwrap();
        let properties = object.get_own_property_names(scope).unwrap();
        let class_entry = ext_php_rs::zend::ClassEntry::try_find("V8Object").unwrap();
        let mut zend_object = ext_php_rs::types::ZendObject::new(class_entry);
        for index in 0..properties.length() {
            let key = properties.get_index(scope, index).unwrap();
            let value = object.get(scope, key).unwrap();

            zend_object
                .set_property(
                    key.to_rust_string_lossy(scope).as_str(),
                    zval_from_jsvalue(value, scope),
                )
                .unwrap();
        }
        return zend_object.into_zval(false).unwrap();
    }
    result.to_rust_string_lossy(scope).try_into().unwrap()
}

pub fn js_value_from_zval<'a>(
    scope: &mut v8::HandleScope<'a>,
    zval: &'_ Zval,
) -> v8::Local<'a, v8::Value> {
    if zval.is_string() {
        return v8::String::new(scope, zval.str().unwrap()).unwrap().into();
    }
    if zval.is_long() || zval.is_double() {
        return v8::Number::new(scope, zval.double().unwrap()).into();
    }
    if zval.is_bool() {
        return v8::Boolean::new(scope, zval.bool().unwrap()).into();
    }
    if zval.is_true() {
        return v8::Boolean::new(scope, true).into();
    }
    if zval.is_false() {
        return v8::Boolean::new(scope, false).into();
    }
    if zval.is_null() {
        return v8::null(scope).into();
    }
    if zval.is_array() {
        let zend_array = zval.array().unwrap();
        let mut values: Vec<v8::Local<'_, v8::Value>> = Vec::new();
        let mut keys: Vec<v8::Local<'_, v8::Name>> = Vec::new();
        let mut has_string_keys = false;
        for (key, elem) in zend_array.iter() {
            let key = match key {
                ArrayKey::String(key) => {
                    has_string_keys = true;
                    key
                },
                ArrayKey::Long(key) => {
                    key.to_string() 
                }
            };
            keys.push(v8::String::new(scope, key.as_str()).unwrap().into());
            values.push(js_value_from_zval(scope, elem));
        }

        if has_string_keys {
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            return v8::Object::with_prototype_and_properties(scope, null, &keys[..], &values[..])
                .into();
        } else {
            return v8::Array::new_with_elements(scope, &values[..]).into();
        }
    }
    // Todo: is_object
    v8::null(scope).into()
}

pub fn op_callback<'scope>(
    scope: &mut deno_core::v8::HandleScope<'scope>,
    args: deno_core::v8::FunctionCallbackArguments,
    mut rv: deno_core::v8::ReturnValue,
) {
    let ctx = unsafe {
        &*(deno_core::v8::Local::<deno_core::v8::External>::cast(args.data().unwrap_unchecked())
            .value() as *const deno_core::_ops::OpCtx)
    };
    let isolate: &mut v8::Isolate = scope.as_mut();
    let callbacks_slot = isolate
        .get_slot::<std::rc::Rc<std::cell::RefCell<HashMap<String, CloneableZval>>>>()
        .unwrap()
        .clone();
    let callbacks = callbacks_slot.borrow_mut();
    let callback_name = ctx.decl.name.to_string();
    let callback = callbacks.get(&callback_name);
    if callback.is_none() {
        // todo: error
        println!("callback not found {:#?}", callback_name);
        return;
    }

    let callback: Zval = callback.unwrap().clone().into_zval(false).unwrap();

    let mut php_args: Vec<CloneableZval> = Vec::new();
    let mut php_args_refs: Vec<&dyn ext_php_rs::convert::IntoZvalDyn> = Vec::new();
    for index in 0..args.length() {
        let v = zval_from_jsvalue(args.get(index), scope);
        let clonable_zval = CloneableZval::from_zval(&v).unwrap();
        php_args.push(clonable_zval);
    }
    for index in 0..php_args.len() {
        php_args_refs.push(php_args.get(index).unwrap());
    }
    let return_value = callback.try_call(php_args_refs).unwrap();
    let return_value_js = js_value_from_zval(scope, &return_value);
    rv.set(return_value_js)
}

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    module
}
