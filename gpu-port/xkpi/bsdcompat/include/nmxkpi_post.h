/* Included after LinuxKPI's compiler headers in every translation unit of the port. */
#ifndef NMXKPI_POST_H
#define NMXKPI_POST_H
/* Xcode's clang rejects __typeof__ on a flexible array annotated with counted_by; the annotation is a bounds-check
 * hint only, so it is dropped here. */
#undef __counted_by
#define __counted_by(member)
#endif
