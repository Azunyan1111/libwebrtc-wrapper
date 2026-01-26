// Force Chromium libc++ ABI namespace (__Cr)
//
// This header must be included BEFORE any libc++ headers to ensure
// the correct ABI namespace is used. libwebrtc.a is built with Chromium's
// libc++ which uses the __Cr namespace instead of __1.
//
// The macros are defined here to ensure they are processed before
// the libc++ __config header which sets up the namespace.

#ifndef LIBWEBRTC_LIBCXX_ABI_H
#define LIBWEBRTC_LIBCXX_ABI_H

// Override ABI version and namespace BEFORE libc++ headers are included
// This must happen before __config is processed
#define _LIBCPP_ABI_VERSION 2
#define _LIBCPP_ABI_NAMESPACE __Cr
#define _LIBCPP_DISABLE_VISIBILITY_ANNOTATIONS 1

#endif // LIBWEBRTC_LIBCXX_ABI_H
