// glbench: per-draw cost of OpenGL on this GPU, shaped like Minecraft's GL 4.1 chunk pass.
// An offscreen GL 4.1 core context (Apple's GL-on-Metal over the NVMTL plugin) draws <draws> small indexed
// meshes per frame, changing a uniform before each draw (Minecraft sets the chunk offset per section), then
// glFinish. Prints frames/s and microseconds per draw.
//   cc -O2 -o glbench glbench.c -framework OpenGL && ./glbench [draws per frame] [frames] [flat]
#define GL_SILENCE_DEPRECATION
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include <limits.h>

static double now_s(void) {
	static mach_timebase_info_data_t tb;
	if (!tb.denom) mach_timebase_info(&tb);
	return (double)mach_absolute_time() * tb.numer / tb.denom / 1e9;
}

static GLuint shader(GLenum type, const char *src) {
	GLuint s = glCreateShader(type);
	glShaderSource(s, 1, &src, NULL);
	glCompileShader(s);
	GLint ok = 0; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
	if (!ok) { char log[1024]; glGetShaderInfoLog(s, sizeof log, NULL, log); fprintf(stderr, "shader: %s\n", log); exit(1); }
	return s;
}

int main(int argc, char **argv) {
	int draws = 4000, frames = 20;
	for (int i = 1; i < argc && i <= 2; i++) {
		char *end; errno = 0; long value = strtol(argv[i], &end, 10);
		if (errno || !argv[i][0] || *end || value < 1 || value > INT_MAX - 2) {
			fprintf(stderr, "draw and frame counts must be positive integers\n"); return 1;
		}
		if (i == 1) draws = (int)value; else frames = (int)value;
	}
	if (argc > 4 || (argc == 4 && strcmp(argv[3], "flat"))) { fprintf(stderr, "usage: glbench [draws] [frames] [flat]\n"); return 1; }
	CGLPixelFormatAttribute attrs[] = { kCGLPFAOpenGLProfile, (CGLPixelFormatAttribute)kCGLOGLPVersion_GL4_Core,
		kCGLPFAAccelerated, kCGLPFAColorSize, 24, kCGLPFADepthSize, 24, 0 };
	CGLPixelFormatObj pf; GLint npf; CGLContextObj ctx;
	if (CGLChoosePixelFormat(attrs, &pf, &npf) || !pf || CGLCreateContext(pf, NULL, &ctx)) { fprintf(stderr, "no GL 4.1 context\n"); return 1; }
	CGLSetCurrentContext(ctx);
	printf("GL_RENDERER %s\nGL_VERSION %s\n", glGetString(GL_RENDERER), glGetString(GL_VERSION));

	GLuint fbo, rb[2]; glGenFramebuffers(1, &fbo); glGenRenderbuffers(2, rb);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo);
	glBindRenderbuffer(GL_RENDERBUFFER, rb[0]); glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, 1920, 1080);
	glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rb[0]);
	glBindRenderbuffer(GL_RENDERBUFFER, rb[1]); glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH_COMPONENT24, 1920, 1080);
	glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, rb[1]);
	if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) { fprintf(stderr, "fbo incomplete\n"); return 1; }
	glViewport(0, 0, 1920, 1080);

	// argv[3] "flat": per-vertex colours through a flat varying, so the image depends on which vertex provokes
	const int flat = argc > 3 && !strcmp(argv[3], "flat");
	const char *vs = flat ? "#version 410 core\n"
		"layout(location=0) in vec3 pos; uniform vec3 offset; uniform mat4 mvp; flat out vec3 c;\n"
		"void main(){ gl_Position = mvp * vec4(pos + offset, 1.0); int k = gl_VertexID % 4; c = vec3(k == 0, k == 1, k == 2) + 0.25 * float(k == 3); }\n"
		: "#version 410 core\n"
		"layout(location=0) in vec3 pos; uniform vec3 offset; uniform mat4 mvp; out vec3 c;\n"
		"void main(){ gl_Position = mvp * vec4(pos + offset, 1.0); c = fract(offset * 0.37); }\n";
	const char *fs = flat ? "#version 410 core\n flat in vec3 c; out vec4 o; uniform vec4 fog;\n"
		"void main(){ o = vec4(c, 1.0); }\n"
		: "#version 410 core\n in vec3 c; out vec4 o; uniform vec4 fog;\n"
		"void main(){ o = vec4(mix(c, fog.rgb, fog.a), 1.0); }\n";
	GLuint prog = glCreateProgram();
	glAttachShader(prog, shader(GL_VERTEX_SHADER, vs)); glAttachShader(prog, shader(GL_FRAGMENT_SHADER, fs));
	glLinkProgram(prog); glUseProgram(prog);
	GLint linked = 0; glGetProgramiv(prog, GL_LINK_STATUS, &linked);
	if (!linked) { char log[2048]; glGetProgramInfoLog(prog, sizeof log, NULL, log); fprintf(stderr, "link: %s\n", log); return 1; }
	GLint uOff = glGetUniformLocation(prog, "offset"), uMvp = glGetUniformLocation(prog, "mvp"), uFog = glGetUniformLocation(prog, "fog");

	// one shared mesh of 64 quads (a "chunk section"), drawn with a different base vertex window per draw
	enum { QUADS = 64 };
	float *v = malloc(QUADS * 4 * 3 * sizeof(float) * 16); unsigned short *ix = malloc(QUADS * 6 * sizeof *ix);
	if (!v || !ix) { fprintf(stderr, "mesh allocation failed\n"); return 1; }
	for (int s = 0; s < 16; s++)
		for (int q = 0; q < QUADS; q++) {
			float x = (q % 8) * 0.01f, y = (q / 8) * 0.01f + s * 0.001f, *p = v + ((s * QUADS + q) * 4) * 3;
			float quad[12] = { x, y, 0, x + .008f, y, 0, x + .008f, y + .008f, 0, x, y + .008f, 0 };
			for (int k = 0; k < 12; k++) p[k] = quad[k];
		}
	for (int q = 0; q < QUADS; q++) { unsigned short b = q * 4, t[6] = { b, b + 1, b + 2, b, b + 2, b + 3 }; for (int k = 0; k < 6; k++) ix[q * 6 + k] = t[k]; }
	GLuint vao, vbo, ibo; glGenVertexArrays(1, &vao); glBindVertexArray(vao);
	glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo); glBufferData(GL_ARRAY_BUFFER, QUADS * 4 * 3 * sizeof(float) * 16, v, GL_STATIC_DRAW);
	glGenBuffers(1, &ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo); glBufferData(GL_ELEMENT_ARRAY_BUFFER, QUADS * 6 * sizeof *ix, ix, GL_STATIC_DRAW);
	glVertexAttribPointer(0, 3, GL_FLOAT, GL_FALSE, 0, 0); glEnableVertexAttribArray(0);
	const float mvp[16] = { 1.5f, 0, 0, 0, 0, 1.5f, 0, 0, 0, 0, 1, 0, -0.9f, -0.9f, 0, 1 };
	glUniformMatrix4fv(uMvp, 1, GL_FALSE, mvp); glUniform4f(uFog, 0.6f, 0.7f, 0.9f, 0.1f);
	glEnable(GL_DEPTH_TEST);

	double best = 1e9, total = 0;
	for (int f = 0; f < frames + 2; f++) {
		double t0 = now_s();
		glClearColor(0.1f, 0.1f, 0.1f, 1); glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
		for (int d = 0; d < draws; d++) {
			glUniform3f(uOff, (d % 64) * 0.025f, (d / 64 % 64) * 0.025f, 0);
			glDrawElementsInstancedBaseVertex(GL_TRIANGLES, QUADS * 6, GL_UNSIGNED_SHORT, 0, 1, (d % 16) * QUADS * 4);
		}
		glFinish();
		double dt = now_s() - t0;
		if (f >= 2) { total += dt; if (dt < best) best = dt; }   // the first two frames build pipelines
	}
	GLenum err = glGetError();
	// image checksum (FNV-1a over RGBA8), to tell a faster driver build from one that draws something else
	unsigned char *px = malloc(1920 * 1080 * 4); uint64_t h = 14695981039346656037ull;
	if (!px) { fprintf(stderr, "readback allocation failed\n"); return 1; }
	glReadPixels(0, 0, 1920, 1080, GL_RGBA, GL_UNSIGNED_BYTE, px);
	GLenum readerr = glGetError();
	for (size_t i = 0; i < 1920u * 1080u * 4u; i++) h = (h ^ px[i]) * 1099511628211ull;
	double avg = total / frames;
	printf("draws/frame %d  frames %d  avg %.1f ms (%.1f fps)  best %.1f ms  %.1f us/draw  glGetError 0x%x  image %016llx\n",
	       draws, frames, avg * 1e3, 1 / avg, best * 1e3, avg * 1e6 / draws, err, (unsigned long long)h);
	free(px); free(v); free(ix);
	glDeleteBuffers(1, &vbo); glDeleteBuffers(1, &ibo); glDeleteVertexArrays(1, &vao);
	glDeleteProgram(prog); glDeleteRenderbuffers(2, rb); glDeleteFramebuffers(1, &fbo);
	CGLSetCurrentContext(NULL); CGLDestroyContext(ctx); CGLDestroyPixelFormat(pf);
	if (readerr) fprintf(stderr, "readback glGetError 0x%x\n", readerr);
	return err != GL_NO_ERROR || readerr != GL_NO_ERROR;
}
