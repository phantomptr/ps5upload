/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * src/rtl.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#include "rtl.h"
#include <string.h>

static uint16_t le16(const uint8_t *p)
{ return (uint16_t)((uint16_t)p[0] | (uint16_t)p[1] << 8); }
static uint32_t le32(const uint8_t *p)
{ return (uint32_t)p[0] | (uint32_t)p[1]<<8 | (uint32_t)p[2]<<16 | (uint32_t)p[3]<<24; }
static void put32(uint8_t *p, uint32_t v)
{ p[0]=(uint8_t)v; p[1]=(uint8_t)(v>>8); p[2]=(uint8_t)(v>>16); p[3]=(uint8_t)(v>>24); }
/* The RTL8157 and RTL8159 format: 16-byte descriptors on both sides. */
static int v2(enum rtl_chip chip) { return chip==RTL_8159 || chip==RTL_8157; }
static int align_size(size_t n, size_t a, size_t *v)
{
    if (n > SIZE_MAX-(a-1)) return RTL_EINVAL;
    *v=(n+a-1)&~(a-1); return 0;
}
int rtl_get_format(enum rtl_chip chip, struct rtl_format *out)
{
    if (!out) return RTL_EINVAL;
    if (chip==RTL_8156B) *out=(struct rtl_format){24,8,8,4};
    else if (v2(chip)) *out=(struct rtl_format){16,16,16,16};
    else return RTL_EUNSUPPORTED;
    return 0;
}
static uint32_t crc32(const uint8_t *p, size_t n)
{
    uint32_t c=UINT32_MAX; size_t i; unsigned b;
    for (i=0;i<n;i++) {
        c^=p[i];
        for (b=0;b<8;b++) c=(c>>1)^(0xedb88320u & (0u-(c&1u)));
    }
    return ~c;
}
static int encode(enum rtl_chip chip, const struct rtl_frame *frames,
    size_t count, uint8_t *out, size_t cap, size_t *written, int rx)
{
    struct rtl_format f; size_t total=0,i,off=0,h,a,n,wire; int e;
    if (!written) return RTL_EINVAL;
    *written=0;
    if ((e=rtl_get_format(chip,&f))) return e;
    if (!frames || !count || !out) return RTL_EINVAL;
    h=rx?f.rx_header:f.tx_header; a=rx?f.rx_align:f.tx_align;
    /* Bounds count before accessing frames, and keep all additions checked. */
    if (count>cap/(h+14u)) return RTL_ENOSPC;
    for (i=0;i<count;i++) {
        n=frames[i].len;
        if (!frames[i].data || n<14 || n>RTL_MAX_FRAME) return RTL_EINVAL;
        if (!rx && n<60) n=60;
        if ((e=align_size(total,a,&total))) return e;
        wire=n+(rx?4u:0u);
        if (h>cap || wire>cap-h || total>cap-h-wire) return RTL_ENOSPC;
        total+=h+wire;
    }
    for (i=0;i<count;i++) {
        size_t start;
        (void)align_size(off,a,&start);
        memset(out+off,0,start-off+h);
        n=frames[i].len;
        if (!rx && n<60) n=60;
        wire=n+(rx?4u:0u);
        if (rx) put32(out+start,v2(chip)?(uint32_t)wire<<17:(uint32_t)wire);
        else {
            put32(out+start,0xc0000000u|(uint32_t)n);
            if (v2(chip)) {
                put32(out+start+8,(uint32_t)n<<4);
                put32(out+start+12,0xa8000000u);
            }
        }
        memcpy(out+start+h,frames[i].data,frames[i].len);
        if (n>frames[i].len) memset(out+start+h+frames[i].len,0,n-frames[i].len);
        if (rx) put32(out+start+h+n,crc32(frames[i].data,n));
        off=start+h+wire;
    }
    *written=total; return 0;
}
int rtl_tx_encode(enum rtl_chip c,const struct rtl_frame *f,size_t n,
    uint8_t *o,size_t z,size_t *w) { return encode(c,f,n,o,z,w,0); }
int rtl_rx_fixture_encode(enum rtl_chip c,const struct rtl_frame *f,size_t n,
    uint8_t *o,size_t z,size_t *w) { return encode(c,f,n,o,z,w,1); }

/* One pass either validates/counts or publishes. Padding is not required to be
 * zero: Linux aligns the next descriptor pointer, without inspecting pad bytes. */
static int walk(enum rtl_chip chip,const uint8_t *data,size_t len,
    struct rtl_frame_view *views,size_t *count,int rx)
{
    struct rtl_format f; size_t off=0,num=0,h,a,n,end,next,j; uint32_t w; int e;
    if ((e=rtl_get_format(chip,&f))) return e;
    h=rx?f.rx_header:f.tx_header; a=rx?f.rx_align:f.tx_align;
    while (off<len) {
        if (len-off<h) return RTL_ETRUNC;
        w=le32(data+off);
        if (rx) {
            n=v2(chip)?(w>>17):(w&0x7fffu);
            /* All-ones is treated as invalid by the FreeBSD reference. */
            if (n==0x7fff || n<18 || n>RTL_MAX_FRAME+4u) return RTL_EFORMAT;
        } else {
            n=w&0x3ffffu;
            if ((w&~0x3ffffu)!=0xc0000000u || le32(data+off+4)!=0 ||
                n<60 || n>RTL_MAX_FRAME) return RTL_EFORMAT;
            if (v2(chip) &&
                (le32(data+off+8)!=(uint32_t)n<<4 ||
                 le32(data+off+12)!=0xa8000000u)) return RTL_EFORMAT;
        }
        if (n>len-off-h) return RTL_ETRUNC;
        end=off+h+n;
        if (views) {
            struct rtl_frame_view *v=&views[num];
            memset(v,0,sizeof(*v));
            v->data=data+off+h; v->len=n-(rx?4u:0u);
            for (j=0;j<h/4;j++) v->status[j]=le32(data+off+j*4);
            if (rx) {
                uint32_t flags=le32(data+off+4);
                v->vlan_present=(uint8_t)(v2(chip)?!!(w&8u):!!(flags&0x10000u));
                if (v2(chip)) flags>>=16;
                v->vlan_tci=(uint16_t)((flags&255u)<<8 | ((flags>>8)&255u));
                if (!v->vlan_present) v->vlan_tci=0;
            }
        }
        num++;
        if (end==len) break;
        if ((e=align_size(end,a,&next))) return e;
        if (len<=next) break; /* optional final padding, including partial pad */
        off=next;
    }
    *count=num; return 0;
}
static int decode(enum rtl_chip c,const uint8_t *data,size_t len,
    struct rtl_frame_view *views,size_t cap,size_t *count,int rx)
{
    size_t n=0; int e;
    if (!count) return RTL_EINVAL;
    *count=0;
    if (!data || !len || (!views && cap)) return RTL_EINVAL;
    if ((e=walk(c,data,len,NULL,&n,rx))) return e;
    if (n>cap) { *count=n; return RTL_ENOSPC; }
    return walk(c,data,len,views,count,rx);
}
int rtl_tx_decode(enum rtl_chip c,const uint8_t *d,size_t n,
    struct rtl_frame_view *f,size_t z,size_t *w) { return decode(c,d,n,f,z,w,0); }
int rtl_rx_decode(enum rtl_chip c,const uint8_t *d,size_t n,
    struct rtl_frame_view *f,size_t z,size_t *w) { return decode(c,d,n,f,z,w,1); }

int rtl_parse_device(const uint8_t *d,size_t n,struct rtl_usb_device *out)
{
    struct rtl_usb_device v;
    if (!d || !out) return RTL_EINVAL;
    if (n!=18) return RTL_ETRUNC;
    if (d[0]!=18 || d[1]!=1 || !d[17] ||
        !(d[7]==8 || d[7]==16 || d[7]==32 || d[7]==64 || d[7]==9)) return RTL_EFORMAT;
    v.bcd_usb=le16(d+2);
    if ((v.bcd_usb>=0x300 && d[7]!=9) ||
        (v.bcd_usb<0x300 && d[7]==9)) return RTL_EFORMAT;
    v.vid=le16(d+8); v.pid=le16(d+10);
    v.bcd_device=le16(d+12); v.configurations=d[17];
    *out=v; return 0;
}
int rtl_identify_target(const struct rtl_usb_device *d,uint16_t ver,
    enum rtl_chip *out)
{
    if (!d || !out) return RTL_EINVAL;
    ver&=0x7cf0u;
    if (d->vid!=0x0bda) return RTL_EUNSUPPORTED;
    /* RTL_VER_12, 13 and 15 in Linux's r8152; 0x7020/0x7030, the first RTL8156, are not.
     * RTL_VER_16 is the RTL8157, RTL_VER_17 the RTL8159. */
    if (d->pid==0x8156 && (ver==0x7400 || ver==0x7410 || ver==0x7420)) *out=RTL_8156B;
    else if (d->pid==0x8157 && ver==0x1030) *out=RTL_8157;
    else if (d->pid==0x815a && ver==0x2020) *out=RTL_8159;
    else return RTL_EUNSUPPORTED;
    return 0;
}
int rtl_version_request(uint8_t setup[8])
{
    static const uint8_t bytes[8]={0xc0,0x05,0x10,0xe6,0x00,0x01,0x04,0x00};
    if (!setup) return RTL_EINVAL;
    memcpy(setup,bytes,sizeof bytes); return 0;
}
int rtl_version_reply(const uint8_t *data,size_t len,uint16_t *version)
{
    if (!data || !version) return RTL_EINVAL;
    if (len!=4) return RTL_ETRUNC;
    *version=(uint16_t)((le32(data)>>16)&0x7cf0u); return 0;
}
int rtl_phy_status_request(uint8_t setup[8])
{
    static const uint8_t bytes[8]={0xc0,0x05,0x08,0xe9,0x00,0x01,0x04,0x00};
    if (!setup) return RTL_EINVAL;
    memcpy(setup,bytes,sizeof bytes); return 0;
}
int rtl_read_request(uint8_t setup[8],unsigned mcu,unsigned address,unsigned size)
{
    if (!setup || (address&3u) || (size&3u) || !size || size>RTL_TALLY_LEN || address>0xffffu) return RTL_EINVAL;
    setup[0]=0xc0; setup[1]=0x05;
    setup[2]=(uint8_t)address; setup[3]=(uint8_t)(address>>8);
    setup[4]=(uint8_t)mcu; setup[5]=(uint8_t)(mcu>>8);
    setup[6]=(uint8_t)size; setup[7]=0;
    return 0;
}
unsigned rtl_byte_write_index(unsigned mcu,unsigned address)
{
    return mcu|(0x11u<<(address&3u));
}
int rtl_tally_reply(const uint8_t *data,size_t len,struct rtl_tally *out)
{
    if (!data || !out) return RTL_EINVAL;
    if (len!=RTL_TALLY_LEN) return RTL_ETRUNC;
    out->tx_packets=(uint64_t)le32(data)|(uint64_t)le32(data+4)<<32;
    out->rx_packets=(uint64_t)le32(data+8)|(uint64_t)le32(data+12)<<32;
    out->tx_errors=(uint64_t)le32(data+16)|(uint64_t)le32(data+20)<<32;
    out->rx_errors=le32(data+24);
    out->rx_missed=le32(data+28)&0xffffu;
    out->tx_aborted=le32(data+60)&0xffffu;
    out->tx_underrun=le32(data+60)>>16;
    return 0;
}
int rtl_write_request(struct rtl_write *out,unsigned mcu,unsigned address,unsigned width,uint32_t value)
{
    unsigned enable,shift=address&3u,index;
    if (!out || (mcu!=RTL_MCU_PLA && mcu!=RTL_MCU_USB) || address>0xffffu) return RTL_EINVAL;
    if (width==1) enable=0x11u<<shift;
    else if (width==2 && !(address&1u)) enable=0x33u<<shift;
    else if (width==4 && !shift) enable=0xffu;
    else return RTL_EINVAL;
    if (width<4 && (value>>(width*8u))) return RTL_EINVAL;
    index=mcu|enable;
    out->setup[0]=0x40; out->setup[1]=0x05;
    out->setup[2]=(uint8_t)(address&~3u); out->setup[3]=(uint8_t)(address>>8);
    out->setup[4]=(uint8_t)index; out->setup[5]=(uint8_t)(index>>8);
    out->setup[6]=4; out->setup[7]=0;
    put32(out->data,value<<(shift*8u));
    return 0;
}
int rtl_tx_header(enum rtl_chip chip,uint8_t *h,size_t len)
{
    struct rtl_format f;
    if (!h || len<60 || len>RTL_MAX_FRAME || rtl_get_format(chip,&f)) return RTL_EINVAL;
    put32(h,0xc0000000u|(uint32_t)len);
    put32(h+4,0);
    if (v2(chip)) {
        put32(h+8,(uint32_t)len<<4);
        put32(h+12,0xa8000000u);
    }
    return (int)f.tx_header;
}
/* Linux's rx_bottom: a length below ETH_ZLEN ends the walk, as it does there. */
int rtl_rx_scan(enum rtl_chip chip,const uint8_t *d,size_t len,struct rtl_frame_view *views,size_t cap,size_t *count)
{
    struct rtl_format f;
    size_t off=0,n=0,j,wire,h,a,least; uint32_t w;
    if (!count) return RTL_EINVAL;
    *count=0;
    if (!d || (!views && cap) || rtl_get_format(chip,&f)) return RTL_EINVAL;
    h=f.rx_header; a=f.rx_align; least=v2(chip)?18u:60u;
    while (off<len) {
        struct rtl_frame_view *v;
        if (len-off<h) return RTL_ETRUNC;
        w=le32(d+off);
        wire=v2(chip)?w>>17:(w&0x7fffu);
        if (wire<least || wire>RTL_MAX_FRAME+4u) return RTL_EFORMAT;
        if (wire>len-off-h) return RTL_ETRUNC;
        if (n==cap) return RTL_ENOSPC;
        v=&views[n];
        memset(v,0,sizeof(*v));
        v->data=d+off+h; v->len=wire-4u;
        for (j=0;j<h/4;j++) v->status[j]=le32(d+off+j*4);
        *count=++n;
        off=(off+h+wire+a-1u)&~(a-1u);
    }
    return 0;
}
int rtl_phy_status_reply(const uint8_t *data,size_t len,uint16_t *status)
{
    if (!data || !status) return RTL_EINVAL;
    if (len!=4) return RTL_ETRUNC;
    *status=(uint16_t)le32(data); return 0;
}
int rtl_phy_link(uint16_t status) { return (status&0x0002u)!=0; }
unsigned rtl_phy_mbps(uint16_t status)
{
    if (!rtl_phy_link(status)) return 0;
    if (status&0x4000u) return 10000;
    if (status&0x1000u) return 5000;
    if (status&0x0400u) return 2500;
    if (status&0x0010u) return 1000;
    if (status&0x0008u) return 100;
    return status&0x0004u ? 10 : 0;
}
int rtl_parse_config(const uint8_t *d,size_t n,uint8_t number,uint8_t alt,
    struct rtl_usb_interface *out)
{
    struct rtl_usb_interface v={0};
    uint8_t interfaces[32]={0}, pairs[8192]={0};
    size_t off=0; unsigned distinct=0,expected=0,seen=0,eps=0,i;
    int have=0,selected=0,found=0;
    if (!d || !out) return RTL_EINVAL;
    if (n<9) return RTL_ETRUNC;
    if (d[0]!=9 || d[1]!=2 || le16(d+2)!=n || !d[5] ||
        !(d[7]&0x80u) || (d[7]&0x1fu)) return RTL_EFORMAT;
    v.configuration=d[5];
    while (off<n) {
        size_t z;
        if (n-off<2) return RTL_ETRUNC;
        z=d[off];
        if (z<2) return RTL_EFORMAT;
        if (z>n-off) return RTL_ETRUNC;
        if (d[off+1]==2 && off!=0) return RTL_EFORMAT;
        if (d[off+1]==4) {
            unsigned key,in,an;
            if (z!=9 || (have && seen!=expected)) return RTL_EFORMAT;
            in=d[off+2]; an=d[off+3]; key=in*256u+an;
            if (in>=d[4]) return RTL_EFORMAT;
            if (pairs[key/8]&(1u<<(key%8))) return RTL_EFORMAT;
            pairs[key/8]|=(uint8_t)(1u<<(key%8));
            if (!(interfaces[in/8]&(1u<<(in%8)))) distinct++;
            interfaces[in/8]|=(uint8_t)(1u<<(in%8));
            have=1; expected=d[off+4]; seen=0; eps=0;
            selected=in==number && an==alt;
            if (selected) {
                found=1; v.number=(uint8_t)in; v.alternate=(uint8_t)an;
                v.class_code=d[off+5]; v.subclass=d[off+6]; v.protocol=d[off+7];
            }
        } else if (d[off+1]==5) {
            uint8_t address,type; uint16_t mps; unsigned bit;
            if (!have || (z!=7 && z!=9)) return RTL_EFORMAT;
            address=d[off+2]; type=d[off+3]&3u; mps=le16(d+off+4);
            if (!(address&15u) || (address&0x70u) || type==0 ||
                !mps || (mps&0xe000u)) return RTL_EFORMAT;
            bit=(address&15u)+((address&0x80u)?16u:0u);
            if (eps&(1u<<bit)) return RTL_EFORMAT;
            eps|=1u<<bit; seen++;
            if (seen>expected) return RTL_EFORMAT;
            if (type==2 && (d[off+3]!=2 ||
                !(mps==8 || mps==16 || mps==32 || mps==64 ||
                  mps==512 || mps==1024))) return RTL_EFORMAT;
            if (selected && type==2) {
                if (address&0x80u) {
                    if (v.bulk_in) return RTL_EFORMAT;
                    v.bulk_in=address; v.bulk_in_mps=mps;
                } else {
                    if (v.bulk_out) return RTL_EFORMAT;
                    v.bulk_out=address; v.bulk_out_mps=mps;
                }
            } else if (selected && type==3 && (address&0x80u)) {
                if (v.interrupt_in) return RTL_EFORMAT;
                v.interrupt_in=address;
            }
        }
        off+=z;
    }
    if (!have || seen!=expected || distinct!=d[4]) return RTL_EFORMAT;
    /* Every interface must supply alternate setting zero. */
    for (i=0;i<d[4];i++) if (!(pairs[i*32u]&1u)) return RTL_EFORMAT;
    if (!found || !v.bulk_in || !v.bulk_out) return RTL_EUNSUPPORTED;
    *out=v; return 0;
}
