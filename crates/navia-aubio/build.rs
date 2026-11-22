//! builds the vendored aubio sources (`aubio/`, upstream commit in `aubio/VENDORED`) into a static
//! library, single precision with aubio's own FFT, so nothing is needed on the system.

const SOURCES: &[&str] = &[
    "cvec.c",
    "fmat.c",
    "fvec.c",
    "lvec.c",
    "mathutils.c",
    "musicutils.c",
    "vecutils.c",
    "onset/onset.c",
    "onset/peakpicker.c",
    "spectral/awhitening.c",
    "spectral/fft.c",
    "spectral/ooura_fft8g.c",
    "spectral/phasevoc.c",
    "spectral/specdesc.c",
    "spectral/statistics.c",
    "tempo/beattracking.c",
    "tempo/tempo.c",
    "temporal/biquad.c",
    "temporal/filter.c",
    "utils/hist.c",
    "utils/log.c",
    "utils/scale.c",
];

const HEADERS: &[&str] = &["HAVE_STDLIB_H", "HAVE_STDIO_H", "HAVE_MATH_H", "HAVE_STRING_H", "HAVE_ERRNO_H", "HAVE_LIMITS_H", "HAVE_STDARG_H", "HAVE_C99_VARARGS_MACROS", "HAVE_MEMCPY_HACKS"];

fn main() {
    let src = std::path::Path::new("aubio/src");
    let mut build = cc::Build::new();
    build.include(src).flag_if_supported("-std=c99").warnings(false);
    for define in HEADERS {
        build.define(define, None);
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.define("HAVE_WIN_HACKS", None);
    }
    for file in SOURCES {
        build.file(src.join(file));
    }
    build.compile("aubio");
    println!("cargo:rerun-if-changed=aubio");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=m");
    }
}
