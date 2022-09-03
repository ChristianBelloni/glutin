//! Everything related to `EGLContext` management.

use std::ffi::{self, CStr};
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;

use glutin_egl_sys::egl::types::EGLint;
use glutin_egl_sys::{egl, EGLContext};

use crate::config::GetGlConfig;
use crate::context::{
    AsRawContext, ContextApi, ContextAttributes, GlProfile, RawContext, Robustness, Version,
};
use crate::display::GetGlDisplay;
use crate::error::{ErrorKind, Result};
use crate::prelude::*;
use crate::private::Sealed;
use crate::surface::SurfaceTypeTrait;

use super::config::Config;
use super::display::Display;
use super::surface::Surface;

impl Display {
    pub(crate) unsafe fn create_context(
        &self,
        config: &Config,
        context_attributes: &ContextAttributes,
    ) -> Result<NotCurrentContext> {
        let mut attrs = Vec::<EGLint>::new();

        let supports_opengl = self.inner.version > Version::new(1, 3);

        let (api, version) = match context_attributes.api {
            ContextApi::OpenGl(version) if supports_opengl => (egl::OPENGL_API, version),
            ContextApi::Gles(version) => (egl::OPENGL_ES_API, version),
            _ => {
                return Err(
                    ErrorKind::NotSupported("the requested context Api isn't supported.").into()
                )
            },
        };

        let is_one_five = self.inner.version >= Version::new(1, 5);
        if is_one_five || self.inner.client_extensions.contains("EGL_KHR_create_context") {
            let mut flags = 0;

            // Add profile for the OpenGL Api.
            if api == egl::OPENGL_API {
                let profile = match context_attributes.profile {
                    Some(GlProfile::Core) | None => egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
                    Some(GlProfile::Compatibility) => egl::CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT,
                };

                attrs.push(egl::CONTEXT_OPENGL_PROFILE_MASK as EGLint);
                attrs.push(profile as EGLint);
            }

            if let Some(version) = version {
                attrs.push(egl::CONTEXT_MAJOR_VERSION as EGLint);
                attrs.push(version.major as EGLint);
                attrs.push(egl::CONTEXT_MINOR_VERSION as EGLint);
                attrs.push(version.minor as EGLint);
            }

            let has_robustsess = is_one_five
                || self.inner.client_extensions.contains("EGL_EXT_create_context_robustness");
            let has_no_error =
                self.inner.client_extensions.contains("EGL_KHR_create_context_no_error");

            match context_attributes.robustness {
                Robustness::NotRobust => (),
                Robustness::NoError if has_no_error => {
                    attrs.push(egl::CONTEXT_OPENGL_NO_ERROR_KHR as EGLint);
                    attrs.push(egl::TRUE as EGLint);
                },
                Robustness::RobustLoseContextOnReset if has_robustsess => {
                    attrs.push(egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as EGLint);
                    attrs.push(egl::LOSE_CONTEXT_ON_RESET as EGLint);
                    flags |= egl::CONTEXT_OPENGL_ROBUST_ACCESS;
                },
                Robustness::RobustNoResetNotification if has_robustsess => {
                    attrs.push(egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY as EGLint);
                    attrs.push(egl::NO_RESET_NOTIFICATION as EGLint);
                    flags |= egl::CONTEXT_OPENGL_ROBUST_ACCESS;
                },
                _ => {
                    return Err(
                        ErrorKind::NotSupported("context robustness is not supported").into()
                    )
                },
            }

            if context_attributes.debug && is_one_five && !has_no_error {
                attrs.push(egl::CONTEXT_OPENGL_DEBUG as EGLint);
                attrs.push(egl::TRUE as EGLint);
            }

            if flags != 0 {
                attrs.push(egl::CONTEXT_FLAGS_KHR as EGLint);
                attrs.push(flags as EGLint);
            }
        }

        attrs.push(egl::NONE as EGLint);

        let shared_context = if let Some(shared_context) =
            context_attributes.shared_context.as_ref()
        {
            match shared_context {
                RawContext::Egl(shared_context) => *shared_context,
                #[allow(unreachable_patterns)]
                _ => return Err(ErrorKind::NotSupported("passed incompatible raw context").into()),
            }
        } else {
            egl::NO_CONTEXT
        };

        // Bind the api.
        unsafe {
            if self.inner.egl.BindAPI(api) == egl::FALSE {
                return Err(super::check_error().err().unwrap());
            }

            let config = config.clone();
            let context = self.inner.egl.CreateContext(
                *self.inner.raw,
                *config.inner.raw,
                shared_context,
                attrs.as_ptr(),
            );

            if context == egl::NO_CONTEXT {
                return Err(super::check_error().err().unwrap());
            }

            let inner = ContextInner { display: self.clone(), config, raw: EglContext(context) };
            Ok(NotCurrentContext::new(inner))
        }
    }
}

/// A wrapper around `EGLContext` that is known to be not current.
#[derive(Debug)]
pub struct NotCurrentContext {
    inner: ContextInner,
}

impl NotCurrentContext {
    fn new(inner: ContextInner) -> Self {
        Self { inner }
    }
}

impl NotCurrentGlContext for NotCurrentContext {
    type PossiblyCurrentContext = PossiblyCurrentContext;

    fn treat_as_current(self) -> Self::PossiblyCurrentContext {
        PossiblyCurrentContext { inner: self.inner, _nosendsync: PhantomData }
    }
}

impl<T: SurfaceTypeTrait> NotCurrentGlContextSurfaceAccessor<T> for NotCurrentContext {
    type PossiblyCurrentContext = PossiblyCurrentContext;
    type Surface = Surface<T>;

    fn make_current(self, surface: &Surface<T>) -> Result<PossiblyCurrentContext> {
        self.inner.make_current_draw_read(surface, surface)?;
        Ok(PossiblyCurrentContext { inner: self.inner, _nosendsync: PhantomData })
    }

    fn make_current_draw_read(
        self,
        surface_draw: &Surface<T>,
        surface_read: &Surface<T>,
    ) -> Result<PossiblyCurrentContext> {
        self.inner.make_current_draw_read(surface_draw, surface_read)?;
        Ok(PossiblyCurrentContext { inner: self.inner, _nosendsync: PhantomData })
    }
}

impl GetGlConfig for NotCurrentContext {
    type Target = Config;

    fn config(&self) -> Self::Target {
        self.inner.config.clone()
    }
}

impl GetGlDisplay for NotCurrentContext {
    type Target = Display;

    fn display(&self) -> Self::Target {
        self.inner.display.clone()
    }
}

impl AsRawContext for NotCurrentContext {
    fn raw_context(&self) -> RawContext {
        RawContext::Egl(*self.inner.raw)
    }
}

impl Sealed for NotCurrentContext {}

/// A wrapper around `EGLContext` that could be current for the current thread.
#[derive(Debug)]
pub struct PossiblyCurrentContext {
    inner: ContextInner,
    _nosendsync: PhantomData<EGLContext>,
}

impl PossiblyCurrentGlContext for PossiblyCurrentContext {
    type NotCurrentContext = NotCurrentContext;

    fn make_not_current(self) -> Result<Self::NotCurrentContext> {
        self.inner.make_not_current()?;
        Ok(NotCurrentContext::new(self.inner))
    }

    fn is_current(&self) -> bool {
        unsafe { self.inner.display.inner.egl.GetCurrentContext() == *self.inner.raw }
    }

    fn get_proc_address(&self, addr: &CStr) -> *const ffi::c_void {
        unsafe { self.inner.display.inner.egl.GetProcAddress(addr.as_ptr()) as *const _ }
    }
}

impl<T: SurfaceTypeTrait> PossiblyCurrentContextGlSurfaceAccessor<T> for PossiblyCurrentContext {
    type Surface = Surface<T>;

    fn make_current(&self, surface: &Self::Surface) -> Result<()> {
        self.inner.make_current_draw_read(surface, surface)
    }

    fn make_current_draw_read(
        &self,
        surface_draw: &Self::Surface,
        surface_read: &Self::Surface,
    ) -> Result<()> {
        self.inner.make_current_draw_read(surface_draw, surface_read)
    }
}

impl GetGlConfig for PossiblyCurrentContext {
    type Target = Config;

    fn config(&self) -> Self::Target {
        self.inner.config.clone()
    }
}

impl GetGlDisplay for PossiblyCurrentContext {
    type Target = Display;

    fn display(&self) -> Self::Target {
        self.inner.display.clone()
    }
}

impl AsRawContext for PossiblyCurrentContext {
    fn raw_context(&self) -> RawContext {
        RawContext::Egl(*self.inner.raw)
    }
}

impl Sealed for PossiblyCurrentContext {}

struct ContextInner {
    display: Display,
    config: Config,
    raw: EglContext,
}

impl ContextInner {
    fn make_current_draw_read<T: SurfaceTypeTrait>(
        &self,
        surface_draw: &Surface<T>,
        surface_read: &Surface<T>,
    ) -> Result<()> {
        unsafe {
            let draw = surface_draw.raw;
            let read = surface_read.raw;
            if self.display.inner.egl.MakeCurrent(*self.display.inner.raw, draw, read, *self.raw)
                == egl::FALSE
            {
                super::check_error()
            } else {
                Ok(())
            }
        }
    }

    fn make_not_current(&self) -> Result<()> {
        unsafe {
            if self.display.inner.egl.MakeCurrent(
                *self.display.inner.raw,
                egl::NO_SURFACE,
                egl::NO_SURFACE,
                egl::NO_CONTEXT,
            ) == egl::FALSE
            {
                super::check_error()
            } else {
                Ok(())
            }
        }
    }
}

impl Drop for ContextInner {
    fn drop(&mut self) {
        unsafe {
            self.display.inner.egl.DestroyContext(*self.display.inner.raw, *self.raw);
        }
    }
}

impl fmt::Debug for ContextInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("display", &self.display.inner.raw)
            .field("config", &self.config.inner.raw)
            .field("raw", &self.raw)
            .finish()
    }
}

#[derive(Debug)]
struct EglContext(EGLContext);

// Impl only `Send` for EglContext.
unsafe impl Send for EglContext {}

impl Deref for EglContext {
    type Target = EGLContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}