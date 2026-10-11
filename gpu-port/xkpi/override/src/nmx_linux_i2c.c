/*
 * I2C adapters for the macOS port (replaces LinuxKPI's linux_i2c.c and linux_i2cbb.c, which attach FreeBSD iicbus
 * devices so FreeBSD userland can reach each bus).
 *
 * In the kernel, i2c_transfer() already calls an adapter's own master_xfer (the GPU's hardware I2C engines: the
 * SMU's EEPROM bus, the display engine's DDC). What remains is the bit-banged bus a driver builds from GPIO lines
 * (i2c-algo-bit: setsda/setscl/getsda/getscl), which FreeBSD runs through its iicbb driver; here it runs through a
 * small master written for this port: standard-mode START / address / data / ACK / STOP, with clock stretching.
 * Registering an adapter has nothing to publish on macOS and only validates it.
 */
#include <sys/param.h>
#include <sys/systm.h>

#include <linux/kernel.h>
#include <linux/delay.h>
#include <linux/jiffies.h>
#include <linux/i2c.h>
#include <linux/i2c-algo-bit.h>

int lkpi_i2c_add_adapter(struct i2c_adapter *adapter)
{
    if (adapter == NULL || adapter->name[0] == '\0')
        return -EINVAL;
    return 0;
}

int lkpi_i2c_del_adapter(struct i2c_adapter *adapter)
{
    return adapter == NULL ? -EINVAL : 0;
}

int lkpi_i2c_bit_add_bus(struct i2c_adapter *adapter)
{
    struct i2c_algo_bit_data *b;

    if (adapter == NULL || (b = adapter->algo_data) == NULL)
        return -EINVAL;
    if (b->setsda == NULL || b->setscl == NULL || b->getsda == NULL)
        return -EINVAL;
    return lkpi_i2c_add_adapter(adapter);
}

/* ---- the bit-banged master ------------------------------------------------------------------------------------------ */

static void half(struct i2c_algo_bit_data *b)
{
    udelay(b->udelay > 0 ? b->udelay : 5);     /* 5 us half period = 100 kHz standard mode */
}

static void sda(struct i2c_algo_bit_data *b, int v)
{
    b->setsda(b->data, v);
    half(b);
}

static void scl_lo(struct i2c_algo_bit_data *b)
{
    b->setscl(b->data, 0);
    half(b);
}

/* raises SCL and waits while a slave holds it low (clock stretching), up to the adapter's timeout */
static int scl_hi(struct i2c_algo_bit_data *b)
{
    b->setscl(b->data, 1);
    if (b->getscl != NULL) {
        unsigned long deadline = jiffies + (b->timeout > 0 ? (unsigned long)b->timeout : (unsigned long)(hz / 10));
        while (!b->getscl(b->data)) {
            if (time_after(jiffies, deadline))
                return -ETIMEDOUT;
            udelay(1);
        }
    }
    half(b);
    return 0;
}

static int start(struct i2c_algo_bit_data *b)
{
    sda(b, 1);
    int r = scl_hi(b);
    if (r)
        return r;
    sda(b, 0);
    scl_lo(b);
    return 0;
}

static void stop(struct i2c_algo_bit_data *b)
{
    sda(b, 0);
    (void)scl_hi(b);
    sda(b, 1);
}

/* sends a byte; 0 when the slave acknowledged it, -ENXIO when it did not */
static int put_byte(struct i2c_algo_bit_data *b, uint8_t v)
{
    for (int i = 7; i >= 0; i--) {
        sda(b, (v >> i) & 1);
        int r = scl_hi(b);
        if (r)
            return r;
        scl_lo(b);
    }
    sda(b, 1);                          /* release SDA for the slave's ACK */
    int r = scl_hi(b);
    if (r)
        return r;
    int nack = b->getsda(b->data);
    scl_lo(b);
    return nack ? -ENXIO : 0;
}

static int get_byte(struct i2c_algo_bit_data *b, uint8_t *v, int ack)
{
    uint8_t x = 0;
    sda(b, 1);
    for (int i = 0; i < 8; i++) {
        int r = scl_hi(b);
        if (r)
            return r;
        x = (uint8_t)((x << 1) | (b->getsda(b->data) ? 1 : 0));
        scl_lo(b);
    }
    sda(b, ack ? 0 : 1);                /* ACK every byte but the last */
    int r = scl_hi(b);
    if (r)
        return r;
    scl_lo(b);
    sda(b, 1);
    *v = x;
    return 0;
}

/* Linux's convention: the number of messages transferred, or a negative errno */
int lkpi_i2cbb_transfer(struct i2c_adapter *adapter, struct i2c_msg *msgs, int nmsgs)
{
    struct i2c_algo_bit_data *b = adapter->algo_data;
    int r = 0;

    if (b == NULL)
        return -EOPNOTSUPP;
    if (b->pre_xfer != NULL && (r = b->pre_xfer(adapter)) < 0)
        return r;
    for (int i = 0; i < nmsgs && r == 0; i++) {
        struct i2c_msg *m = &msgs[i];
        int rd = (m->flags & I2C_M_RD) != 0;
        if (i == 0 || !(m->flags & I2C_M_NOSTART)) {
            r = start(b);
            if (r == 0)
                r = put_byte(b, (uint8_t)((m->addr << 1) | rd));
            if (r)
                break;
        }
        for (int j = 0; j < m->len && r == 0; j++) {
            if (rd)
                r = get_byte(b, &m->buf[j], j + 1 < m->len);
            else if ((r = put_byte(b, m->buf[j])) == -ENXIO)
                r = -EIO;               /* a data byte refused, as opposed to no device at the address */
        }
    }
    stop(b);
    if (b->post_xfer != NULL)
        b->post_xfer(adapter);
    return r ? r : nmsgs;
}
