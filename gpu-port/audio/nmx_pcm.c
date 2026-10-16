/*
 * The port's sound core: the part of FreeBSD's sys/dev/sound/pcm that a hardware driver calls (pcm_*, sndbuf_*,
 * mixer_init, mix_*, chn_intr), with the macOS audio engine above it instead of /dev/dsp. FreeBSD's core adds feeders
 * (format and rate conversion, mixing of virtual channels); CoreAudio does that itself, so a channel here runs at a
 * format and rate the codec accepts and the engine converts into it.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/kernel.h>
#include <dev/sound/pcm/sound.h>
#include "channel_if.h"
#include "mixer_if.h"
#include "nmx_pcm.h"

MALLOC_DECLARE(M_NMXSND);

struct pcm_channel {
	kobj_t obj;			/* the driver's channel class instance */
	void *data;			/* what CHANNEL_INIT returned */
	struct snd_dbuf buf;
	int dir;
	struct snddev_info *sc;
	uint32_t format, speed;
	int running;
	void *engine;			/* set by the engine; chn_intr reports blocks to it */
};

int snd_unit = -1;

/* ---- kobj: methods by name (device methods are bsdcompat's; this is the same lookup for the sound classes) */

void *nmx_kobj_method(kobj_class_t cls, const char *name)
{
	if (cls == NULL)
		return NULL;
	for (kobj_method_t *m = cls->methods; m != NULL && m->name != NULL; m++)
		if (strcmp(m->name, name) == 0)
			return m->func;
	return NULL;
}

kobj_t nmx_kobj_create(kobj_class_t cls)
{
	size_t size = cls->size < sizeof(struct kobj) ? sizeof(struct kobj) : cls->size;
	kobj_t obj = malloc(size, M_NMXSND, M_WAITOK | M_ZERO);
	obj->ops = cls;
	return obj;
}

void nmx_kobj_delete(kobj_t obj) { free(obj, M_NMXSND); }

/* used by channel_if's default getmatrix (hdaa has its own channel maps and does not call it) */
uint32_t feeder_matrix_default_format(uint32_t format) { return format; }
struct pcmchan_matrix *feeder_matrix_format_map(uint32_t format) { return NULL; }

/* ---- DMA buffers */

static void dbuf_loaded(void *arg, bus_dma_segment_t *segs, int nseg, int error)
{
	struct snd_dbuf *b = arg;
	if (error == 0 && nseg == 1)
		b->buf_addr = segs[0].ds_addr;
}

int sndbuf_alloc(struct snd_dbuf *b, bus_dma_tag_t dmatag, int dmaflags, unsigned int size)
{
	void *mem;
	b->dmatag = dmatag;
	if (bus_dmamem_alloc(dmatag, &mem, BUS_DMA_NOWAIT | BUS_DMA_ZERO | dmaflags, &b->dmamap) != 0)
		return ENOMEM;
	b->buf = mem;
	b->buf_addr = 0;
	if (bus_dmamap_load(dmatag, b->dmamap, mem, size, dbuf_loaded, b, BUS_DMA_NOWAIT) != 0 || b->buf_addr == 0) {
		bus_dmamem_free(dmatag, mem, b->dmamap);
		b->buf = NULL;
		return ENOMEM;
	}
	b->maxsize = b->bufsize = size;
	b->align = 4;
	b->blksz = size;
	b->blkcnt = 1;
	return 0;
}

static void sndbuf_free(struct snd_dbuf *b)
{
	if (b->buf == NULL)
		return;
	bus_dmamap_unload(b->dmatag, b->dmamap);
	bus_dmamem_free(b->dmatag, b->buf, b->dmamap);
	b->buf = NULL;
}

int sndbuf_resize(struct snd_dbuf *b, unsigned int blkcnt, unsigned int blksz)
{
	if (blkcnt == 0 || blksz == 0 || (uint64_t)blkcnt * blksz > b->maxsize)
		return EINVAL;
	b->blkcnt = blkcnt;
	b->blksz = blksz;
	b->bufsize = blkcnt * blksz;
	return 0;
}

bus_addr_t sndbuf_getbufaddr(struct snd_dbuf *b) { return b->buf_addr; }
void *sndbuf_getbuf(struct snd_dbuf *b) { return b->buf; }
unsigned int sndbuf_getsize(struct snd_dbuf *b) { return b->bufsize; }
unsigned int sndbuf_getblksz(struct snd_dbuf *b) { return b->blksz; }
unsigned int sndbuf_getblkcnt(struct snd_dbuf *b) { return b->blkcnt; }

/* ---- PCM devices and channels */

static struct snddev_info *softc(device_t dev) { return device_get_softc(dev); }

void pcm_init(device_t dev, void *devinfo)
{
	struct snddev_info *sc = softc(dev);
	struct sysctl_oid *tree = device_get_sysctl_tree(dev);
	sc->dev = dev;
	sc->devinfo = devinfo;
	sysctl_ctx_init(&sc->play_sysctl_ctx);
	sysctl_ctx_init(&sc->rec_sysctl_ctx);
	sc->play_sysctl_tree = SYSCTL_ADD_NODE(&sc->play_sysctl_ctx, SYSCTL_CHILDREN(tree), OID_AUTO, "play",
	    CTLFLAG_RD | CTLFLAG_MPSAFE, NULL, "playback channels");
	sc->rec_sysctl_tree = SYSCTL_ADD_NODE(&sc->rec_sysctl_ctx, SYSCTL_CHILDREN(tree), OID_AUTO, "rec",
	    CTLFLAG_RD | CTLFLAG_MPSAFE, NULL, "recording channels");
}

int pcm_addchan(device_t dev, int dir, kobj_class_t cls, void *devinfo)
{
	struct snddev_info *sc = softc(dev);
	if (sc->nchan >= NMX_PCM_MAXCHAN)
		return ENOSPC;
	struct pcm_channel *c = malloc(sizeof(*c), M_NMXSND, M_WAITOK | M_ZERO);
	c->obj = nmx_kobj_create(cls);
	c->dir = dir;
	c->sc = sc;
	c->buf.channel = c;
	c->data = CHANNEL_INIT(c->obj, devinfo, &c->buf, c, dir);
	if (c->data == NULL) {
		device_printf(dev, "channel %d (%s) did not initialise\n", sc->nchan, dir == PCMDIR_PLAY ? "play" : "rec");
		nmx_kobj_delete(c->obj);
		free(c, M_NMXSND);
		return ENODEV;
	}
	sc->chan[sc->nchan++] = c;
	return 0;
}

int pcm_register(device_t dev, char *str)
{
	struct snddev_info *sc = softc(dev);
	strlcpy(sc->status, str ? str : "", sizeof(sc->status));
	sc->registered = 1;
	device_printf(dev, "%s: %d channel(s) %s\n", device_get_desc(dev), sc->nchan, sc->status);
	nmx_audio_publish(sc);
	return 0;
}

int pcm_unregister(device_t dev)
{
	struct snddev_info *sc = softc(dev);
	if (!sc->registered)
		return 0;
	nmx_audio_unpublish(sc);
	for (int i = 0; i < sc->nchan; i++) {
		struct pcm_channel *c = sc->chan[i];
		if (c->running)
			CHANNEL_TRIGGER(c->obj, c->data, PCMTRIG_ABORT);
		CHANNEL_FREE(c->obj, c->data);
		sndbuf_free(&c->buf);
		nmx_kobj_delete(c->obj);
		free(c, M_NMXSND);
	}
	sc->nchan = 0;
	if (sc->mixer != NULL) {
		MIXER_UNINIT(sc->mixer);
		free(sc->mixer, M_NMXSND);
		sc->mixer = NULL;
	}
	sysctl_ctx_free(&sc->play_sysctl_ctx);
	sysctl_ctx_free(&sc->rec_sysctl_ctx);
	sc->registered = 0;
	return 0;
}

unsigned int pcm_getbuffersize(device_t dev, unsigned int minbufsz, unsigned int deflt, unsigned int maxbufsz)
{
	return deflt;
}

uint32_t pcm_getflags(device_t dev) { return softc(dev)->flags; }
void pcm_setflags(device_t dev, uint32_t val) { softc(dev)->flags = val; }

void chn_intr(struct pcm_channel *c)
{
	if (c->engine != NULL)
		nmx_audio_block_done(c->engine, c);
}

int snd_setup_intr(device_t dev, struct resource *res, int flags, driver_intr_t hand, void *param, void **cookiep)
{
	return bus_setup_intr(dev, res, flags | INTR_MPSAFE, NULL, hand, param, cookiep);
}

/* ---- mixer */

int mixer_init(device_t dev, kobj_class_t cls, void *devinfo)
{
	struct snddev_info *sc = softc(dev);
	struct snd_mixer *m = malloc(sizeof(*m), M_NMXSND, M_WAITOK | M_ZERO);
	m->ops = cls;
	m->devinfo = devinfo;
	m->dev = dev;
	for (int i = 0; i < SOUND_MIXER_NRDEVICES; i++)
		m->realdev[i] = i;
	if (MIXER_INIT(m) != 0) {
		free(m, M_NMXSND);
		return ENXIO;
	}
	sc->mixer = m;
	/* FreeBSD's defaults: everything at 75, recording level 0 */
	for (int i = 0; i < SOUND_MIXER_NRDEVICES; i++)
		if (m->devs & (1u << i))
			mix_set(m, i, i == SOUND_MIXER_RECLEV ? 0 : 75, i == SOUND_MIXER_RECLEV ? 0 : 75);
	return 0;
}

int mixer_reinit(device_t dev)
{
	struct snd_mixer *m = softc(dev)->mixer;
	if (m == NULL)
		return ENXIO;
	int error = MIXER_REINIT(m);
	if (error)
		return error;
	for (int i = 0; i < SOUND_MIXER_NRDEVICES; i++)
		if (m->devs & (1u << i))
			MIXER_SET(m, i, m->level[i][0], m->level[i][1]);
	return 0;
}

void *mix_getdevinfo(struct snd_mixer *m) { return m->devinfo; }
void mix_setdevs(struct snd_mixer *m, uint32_t v) { m->devs = v; }
void mix_setrecdevs(struct snd_mixer *m, uint32_t v) { m->recdevs = v; }
uint32_t mix_getdevs(struct snd_mixer *m) { return m->devs; }
uint32_t mix_getrecdevs(struct snd_mixer *m) { return m->recdevs; }

void mix_setparentchild(struct snd_mixer *m, uint32_t parent, uint32_t childs)
{
	if (parent < SOUND_MIXER_NRDEVICES)
		m->child[parent] = childs;
	for (int i = 0; i < SOUND_MIXER_NRDEVICES; i++)
		if (childs & (1u << i))
			m->parent[i] = 1u << parent;
}

void mix_setrealdev(struct snd_mixer *m, uint32_t dev, uint32_t realdev)
{
	if (dev < SOUND_MIXER_NRDEVICES)
		m->realdev[dev] = realdev;
}

int mix_set(struct snd_mixer *m, unsigned int dev, unsigned int left, unsigned int right)
{
	if (dev >= SOUND_MIXER_NRDEVICES)
		return EINVAL;
	left = left > 100 ? 100 : left;
	right = right > 100 ? 100 : right;
	m->level[dev][0] = left;
	m->level[dev][1] = right;
	MIXER_SET(m, dev, left, right);
	return 0;
}

int mix_setrecsrc(struct snd_mixer *m, uint32_t src)
{
	uint32_t got = MIXER_SETRECSRC(m, src);
	m->recsrc = got;
	return got == 0 ? EINVAL : 0;
}

/* ---- the engine's API (nmx_pcm.h) */

int nmx_chan_dir(struct pcm_channel *c) { return c->dir; }

const uint32_t *nmx_chan_formats(struct pcm_channel *c, uint32_t *minspeed, uint32_t *maxspeed)
{
	struct pcmchan_caps *caps = CHANNEL_GETCAPS(c->obj, c->data);
	if (caps == NULL)
		return NULL;
	*minspeed = caps->minspeed;
	*maxspeed = caps->maxspeed;
	return caps->fmtlist;
}

int nmx_chan_setformat(struct pcm_channel *c, uint32_t format)
{
	int error = CHANNEL_SETFORMAT(c->obj, c->data, format);
	if (error == 0)
		c->format = format;
	return error;
}

uint32_t nmx_chan_setspeed(struct pcm_channel *c, uint32_t speed)
{
	c->speed = CHANNEL_SETSPEED(c->obj, c->data, speed);
	return c->speed;
}

int nmx_chan_setfragments(struct pcm_channel *c, uint32_t blksz, uint32_t blkcnt)
{
	return CHANNEL_SETFRAGMENTS(c->obj, c->data, blksz, blkcnt);
}

void *nmx_chan_ring(struct pcm_channel *c, uint32_t *bytes, uint32_t *blksz)
{
	*bytes = c->buf.bufsize;
	*blksz = c->buf.blksz;
	return c->buf.buf;
}

int nmx_chan_start(struct pcm_channel *c)
{
	int error = CHANNEL_TRIGGER(c->obj, c->data, PCMTRIG_START);
	c->running = error == 0;
	return error;
}

int nmx_chan_stop(struct pcm_channel *c)
{
	if (!c->running)
		return 0;
	c->running = 0;
	return CHANNEL_TRIGGER(c->obj, c->data, PCMTRIG_STOP);
}

uint32_t nmx_chan_getptr(struct pcm_channel *c) { return CHANNEL_GETPTR(c->obj, c->data); }
void nmx_chan_set_engine(struct pcm_channel *c, void *engine) { c->engine = engine; }

int nmx_pcm_nchan(struct snddev_info *sc) { return sc->nchan; }
struct pcm_channel *nmx_pcm_chan(struct snddev_info *sc, int i) { return i >= 0 && i < sc->nchan ? sc->chan[i] : NULL; }
const char *nmx_pcm_name(struct snddev_info *sc) { return device_get_desc(sc->dev); }
uint32_t nmx_pcm_mixer_devs(struct snddev_info *sc) { return sc->mixer ? sc->mixer->devs : 0; }

int nmx_pcm_mixer_set(struct snddev_info *sc, unsigned dev, unsigned left, unsigned right)
{
	return sc->mixer ? mix_set(sc->mixer, dev, left, right) : ENXIO;
}
