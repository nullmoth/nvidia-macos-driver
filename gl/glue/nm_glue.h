/*
 * nm_glue.h: what the generated function table and the Apple-extension entries need from the context layer.
 */
#ifndef NM_GLUE_H
#define NM_GLUE_H

#include <OpenGL/gliContext.h>

/*
 * Callers of the table pass the engine context explicitly and may do so from any thread, while Mesa's API layer
 * works on the thread's current context. Every table entry therefore starts here: if the thread's current Mesa
 * context is not the one behind ctx, it is made current. The common case is one compare.
 */
void nm_gl_enter(GLIContext ctx);

/* Records GL_INVALID_OPERATION on ctx for a table entry that has no implementation, and logs the name once. */
void nm_gl_refuse(GLIContext ctx, const char *entry);

#endif
