/***************************************************************************
 * Trimpod: runtime SVG file-type icons for media lists (see trimpod_svg.h).
 ****************************************************************************/
#include <ctype.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef TRIMPOD_SVG_HOST_TEST
#include "config.h"
#include "file.h"
#include "lcd.h"
#include "rbpaths.h"
#include "trimpod_svg.h"
extern void lcd_alpha_bitmap_part(const unsigned char *src, int src_x,
                                  int src_y, int stride, int x, int y,
                                  int width, int height);
#else
#include <fcntl.h>
#include <unistd.h>
#define ROCKBOX_DIR "."
#define TRIMPOD_SVG_ICON_SIZE 56
#define TRIMPOD_SVG_ICON_GAP  12
#define TRIMPOD_SVG_TOGGLE_H  40
struct trimpod_svg_icon {
    char name[24];
    int width;
    int height;
    unsigned char *alpha;
};
#endif

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

#define SVG_MAX_FILE_BYTES  65536
#define SVG_MAX_STACK       32
#define SVG_CACHE_SLOTS     64
#define SVG_KEY_MAX         32
#define SVG_SS              8   /* 8x8 supersampling per pixel */

/* ---- 2D affine matrix: [a c e; b d f; 0 0 1] -------------------------- */

struct svg_mat { float a, b, c, d, e, f; };

static struct svg_mat mat_identity(void)
{
    return (struct svg_mat){ 1.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f };
}

static struct svg_mat mat_mul(struct svg_mat m1, struct svg_mat m2)
{
    return (struct svg_mat){
        m1.a * m2.a + m1.c * m2.b,
        m1.b * m2.a + m1.d * m2.b,
        m1.a * m2.c + m1.c * m2.d,
        m1.b * m2.c + m1.d * m2.d,
        m1.a * m2.e + m1.c * m2.f + m1.e,
        m1.b * m2.e + m1.d * m2.f + m1.f,
    };
}

static void mat_point(struct svg_mat m, float x, float y, float *ox, float *oy)
{
    *ox = m.a * x + m.c * y + m.e;
    *oy = m.b * x + m.d * y + m.f;
}

static float mat_scale_factor(struct svg_mat m)
{
    return sqrtf(0.5f * (m.a * m.a + m.b * m.b + m.c * m.c + m.d * m.d));
}

/* ---- style state ------------------------------------------------------- */

enum svg_paint_kind { PAINT_NONE = 0, PAINT_COLOR };
enum svg_fill_rule  { RULE_NONZERO = 0, RULE_EVENODD };
enum svg_linecap    { CAP_BUTT = 0, CAP_ROUND, CAP_SQUARE };
enum svg_linejoin   { JOIN_MITER = 0, JOIN_ROUND, JOIN_BEVEL };

struct svg_color { uint8_t r, g, b, a; };

struct svg_style
{
    enum svg_paint_kind fill_kind;
    struct svg_color    fill;
    float               fill_opacity;

    enum svg_paint_kind stroke_kind;
    struct svg_color    stroke;
    float               stroke_opacity;
    float               stroke_width;
    float               miter_limit;
    enum svg_linecap    linecap;
    enum svg_linejoin   linejoin;

    enum svg_fill_rule  fill_rule;
    float               opacity;
    struct svg_mat      xform;
};

static struct svg_style style_default(void)
{
    struct svg_style s;
    memset(&s, 0, sizeof(s));
    s.fill_kind      = PAINT_COLOR;
    s.fill           = (struct svg_color){ 0, 0, 0, 255 };
    s.fill_opacity   = 1.0f;
    s.stroke_kind    = PAINT_NONE;
    s.stroke         = (struct svg_color){ 0, 0, 0, 255 };
    s.stroke_opacity = 1.0f;
    s.stroke_width   = 1.0f;
    s.miter_limit    = 4.0f;
    s.linecap        = CAP_BUTT;
    s.linejoin       = JOIN_MITER;
    s.fill_rule      = RULE_NONZERO;
    s.opacity        = 1.0f;
    s.xform          = mat_identity();
    return s;
}

/* ---- flattened path buffer -------------------------------------------- */

struct svg_pt  { float x, y; };
struct svg_sub { int start, count; bool closed; };

struct svg_path_buf
{
    struct svg_pt  *pts;
    int             npts, cap_pts;
    struct svg_sub *subs;
    int             nsubs, cap_subs;
};

static void path_init(struct svg_path_buf *pb)
{
    memset(pb, 0, sizeof(*pb));
}

static void path_free(struct svg_path_buf *pb)
{
    free(pb->pts);
    free(pb->subs);
    memset(pb, 0, sizeof(*pb));
}

static void path_clear(struct svg_path_buf *pb)
{
    pb->npts = 0;
    pb->nsubs = 0;
}

static bool path_begin_sub(struct svg_path_buf *pb, float x, float y)
{
    if (pb->nsubs == pb->cap_subs)
    {
        int nc = pb->cap_subs ? pb->cap_subs * 2 : 16;
        struct svg_sub *ns = realloc(pb->subs, (size_t)nc * sizeof(*ns));
        if (!ns) return false;
        pb->subs = ns;
        pb->cap_subs = nc;
    }
    if (pb->npts == pb->cap_pts)
    {
        int nc = pb->cap_pts ? pb->cap_pts * 2 : 128;
        struct svg_pt *np = realloc(pb->pts, (size_t)nc * sizeof(*np));
        if (!np) return false;
        pb->pts = np;
        pb->cap_pts = nc;
    }
    pb->subs[pb->nsubs++] = (struct svg_sub){ pb->npts, 1, false };
    pb->pts[pb->npts++]   = (struct svg_pt){ x, y };
    return true;
}

static bool path_add_pt(struct svg_path_buf *pb, float x, float y)
{
    if (pb->nsubs == 0)
        return path_begin_sub(pb, x, y);
    struct svg_sub *sub = &pb->subs[pb->nsubs - 1];
    if (sub->count > 0)
    {
        struct svg_pt last = pb->pts[pb->npts - 1];
        float dx = x - last.x, dy = y - last.y;
        if (dx * dx + dy * dy < 1e-6f)
            return true;
    }
    if (pb->npts == pb->cap_pts)
    {
        int nc = pb->cap_pts ? pb->cap_pts * 2 : 128;
        struct svg_pt *np = realloc(pb->pts, (size_t)nc * sizeof(*np));
        if (!np) return false;
        pb->pts = np;
        pb->cap_pts = nc;
    }
    pb->pts[pb->npts++] = (struct svg_pt){ x, y };
    sub->count++;
    return true;
}

static void path_close_sub(struct svg_path_buf *pb)
{
    if (pb->nsubs > 0)
        pb->subs[pb->nsubs - 1].closed = true;
}

static void flatten_cubic(struct svg_path_buf *pb,
                          float x0, float y0, float x1, float y1,
                          float x2, float y2, float x3, float y3, int depth)
{
    if (depth < 8)
    {
        float dx = x3 - x0, dy = y3 - y0;
        float d2 = fabsf((x1 - x3) * dy - (y1 - y3) * dx);
        float d3 = fabsf((x2 - x3) * dy - (y2 - y3) * dx);
        if ((d2 + d3) * (d2 + d3) <= 0.01f * (dx * dx + dy * dy))
        {
            path_add_pt(pb, x3, y3);
            return;
        }
        float x01 = 0.5f * (x0 + x1), y01 = 0.5f * (y0 + y1);
        float x12 = 0.5f * (x1 + x2), y12 = 0.5f * (y1 + y2);
        float x23 = 0.5f * (x2 + x3), y23 = 0.5f * (y2 + y3);
        float x012 = 0.5f * (x01 + x12), y012 = 0.5f * (y01 + y12);
        float x123 = 0.5f * (x12 + x23), y123 = 0.5f * (y12 + y23);
        float x0123 = 0.5f * (x012 + x123), y0123 = 0.5f * (y012 + y123);
        flatten_cubic(pb, x0, y0, x01, y01, x012, y012, x0123, y0123, depth + 1);
        flatten_cubic(pb, x0123, y0123, x123, y123, x23, y23, x3, y3, depth + 1);
        return;
    }
    path_add_pt(pb, x3, y3);
}

static void flatten_quad(struct svg_path_buf *pb,
                         float x0, float y0, float x1, float y1,
                         float x2, float y2)
{
    /* Exact elevation of quadratic Bezier to cubic Bezier */
    float cx1 = x0 + (2.0f / 3.0f) * (x1 - x0);
    float cy1 = y0 + (2.0f / 3.0f) * (y1 - y0);
    float cx2 = x2 + (2.0f / 3.0f) * (x1 - x2);
    float cy2 = y2 + (2.0f / 3.0f) * (y1 - y2);
    flatten_cubic(pb, x0, y0, cx1, cy1, cx2, cy2, x2, y2, 0);
}

static void flatten_arc(struct svg_path_buf *pb, struct svg_mat m,
                        float x0, float y0, float rx, float ry,
                        float x_axis_rot_deg, int large_arc, int sweep,
                        float x1, float y1)
{
    rx = fabsf(rx);
    ry = fabsf(ry);
    if (rx < 1e-5f || ry < 1e-5f || (fabsf(x0 - x1) < 1e-5f && fabsf(y0 - y1) < 1e-5f))
    {
        float tx, ty;
        mat_point(m, x1, y1, &tx, &ty);
        path_add_pt(pb, tx, ty);
        return;
    }

    float phi = x_axis_rot_deg * (float)(M_PI / 180.0);
    float cosphi = cosf(phi), sinphi = sinf(phi);
    float dx2 = 0.5f * (x0 - x1), dy2 = 0.5f * (y0 - y1);
    float x1p =  cosphi * dx2 + sinphi * dy2;
    float y1p = -sinphi * dx2 + cosphi * dy2;

    float rx2 = rx * rx, ry2 = ry * ry;
    float x1p2 = x1p * x1p, y1p2 = y1p * y1p;
    float lam = x1p2 / rx2 + y1p2 / ry2;
    if (lam > 1.0f)
    {
        float s = sqrtf(lam);
        rx *= s; ry *= s;
        rx2 = rx * rx; ry2 = ry * ry;
    }

    float num = rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2;
    float den = rx2 * y1p2 + ry2 * x1p2;
    float sq = (den > 0.0f && num > 0.0f) ? sqrtf(num / den) : 0.0f;
    if (large_arc == sweep) sq = -sq;

    float cxp =  sq * rx * y1p / ry;
    float cyp = -sq * ry * x1p / rx;
    float cx = cosphi * cxp - sinphi * cyp + 0.5f * (x0 + x1);
    float cy = sinphi * cxp + cosphi * cyp + 0.5f * (y0 + y1);

    float ux = (x1p - cxp) / rx, uy = (y1p - cyp) / ry;
    float vx = (-x1p - cxp) / rx, vy = (-y1p - cyp) / ry;
    float th1 = atan2f(uy, ux);
    float dth = atan2f(ux * vy - uy * vx, ux * vx + uy * vy);
    if (!sweep && dth > 0.0f) dth -= (float)(2.0 * M_PI);
    if (sweep && dth < 0.0f)  dth += (float)(2.0 * M_PI);

    int segs = (int)ceilf(fabsf(dth) / (float)(M_PI * 0.5));
    if (segs < 1) segs = 1;
    float step = dth / (float)segs;
    float t = (4.0f / 3.0f) * tanf(0.25f * step);

    for (int i = 0; i < segs; i++)
    {
        float a0 = th1 + (float)i * step;
        float a1 = a0 + step;
        float ca0 = cosf(a0), sa0 = sinf(a0);
        float ca1 = cosf(a1), sa1 = sinf(a1);

        float p0x = cx + cosphi * rx * ca0 - sinphi * ry * sa0;
        float p0y = cy + sinphi * rx * ca0 + cosphi * ry * sa0;
        float p1x = p0x + t * (-cosphi * rx * sa0 - sinphi * ry * ca0);
        float p1y = p0y + t * (-sinphi * rx * sa0 + cosphi * ry * ca0);
        float p3x = cx + cosphi * rx * ca1 - sinphi * ry * sa1;
        float p3y = cy + sinphi * rx * ca1 + cosphi * ry * sa1;
        float p2x = p3x - t * (-cosphi * rx * sa1 - sinphi * ry * ca1);
        float p2y = p3y - t * (-sinphi * rx * sa1 + cosphi * ry * ca1);

        float q0x, q0y, q1x, q1y, q2x, q2y, q3x, q3y;
        mat_point(m, p0x, p0y, &q0x, &q0y);
        mat_point(m, p1x, p1y, &q1x, &q1y);
        mat_point(m, p2x, p2y, &q2x, &q2y);
        mat_point(m, p3x, p3y, &q3x, &q3y);
        flatten_cubic(pb, q0x, q0y, q1x, q1y, q2x, q2y, q3x, q3y, 0);
    }
}

/* ---- stroke expansion into closed filled subpaths --------------------- */

static void add_arc_fan(struct svg_path_buf *out, float cx, float cy,
                        float a0, float a1, float r)
{
    float da = a1 - a0;
    while (da >  (float)M_PI) da -= (float)(2.0 * M_PI);
    while (da < -(float)M_PI) da += (float)(2.0 * M_PI);
    int steps = (int)ceilf(fabsf(da) / 0.35f);
    if (steps < 1) steps = 1;
    for (int i = 1; i <= steps; i++)
    {
        float a = a0 + da * ((float)i / (float)steps);
        path_add_pt(out, cx + cosf(a) * r, cy + sinf(a) * r);
    }
}

static void add_join(struct svg_path_buf *out, struct svg_pt p,
                     float nx0, float ny0, float nx1, float ny1,
                     float hw, enum svg_linejoin join, float miter_limit)
{
    float cross = nx0 * ny1 - ny0 * nx1;
    if (fabsf(cross) < 1e-4f && (nx0 * nx1 + ny0 * ny1) > 0.0f)
    {
        path_add_pt(out, p.x + nx1 * hw, p.y + ny1 * hw);
        return;
    }
    if (join == JOIN_ROUND)
    {
        float a0 = atan2f(ny0, nx0);
        float a1 = atan2f(ny1, nx1);
        path_add_pt(out, p.x + nx0 * hw, p.y + ny0 * hw);
        add_arc_fan(out, p.x, p.y, a0, a1, hw);
        return;
    }
    if (join == JOIN_MITER)
    {
        float bx = nx0 + nx1, by = ny0 + ny1;
        float bl2 = bx * bx + by * by;
        if (bl2 > 1e-6f)
        {
            float scale = 2.0f / bl2;
            float mlen = sqrtf(bl2 * scale * scale);
            if (mlen <= miter_limit)
            {
                path_add_pt(out, p.x + bx * scale * hw, p.y + by * scale * hw);
                return;
            }
        }
    }
    /* Bevel fallback */
    path_add_pt(out, p.x + nx0 * hw, p.y + ny0 * hw);
    path_add_pt(out, p.x + nx1 * hw, p.y + ny1 * hw);
}

static void expand_stroke(const struct svg_path_buf *in,
                          float stroke_w, enum svg_linecap cap,
                          enum svg_linejoin join, float miter_limit,
                          struct svg_path_buf *out)
{
    path_clear(out);
    float hw = 0.5f * stroke_w;
    if (hw < 0.05f)
        return;

    for (int s = 0; s < in->nsubs; s++)
    {
        struct svg_sub sub = in->subs[s];
        const struct svg_pt *pts = in->pts + sub.start;
        int n = sub.count;
        if (n < 2)
            continue;

        /* Drop duplicate closing vertex if present */
        if (sub.closed && n > 2)
        {
            float dx = pts[n - 1].x - pts[0].x;
            float dy = pts[n - 1].y - pts[0].y;
            if (dx * dx + dy * dy < 1e-5f)
                n--;
        }
        if (n < 2)
            continue;

        int nsegs = sub.closed ? n : (n - 1);
        float *nx = malloc((size_t)nsegs * sizeof(float));
        float *ny = malloc((size_t)nsegs * sizeof(float));
        if (!nx || !ny) { free(nx); free(ny); return; }

        bool any_valid = false;
        for (int i = 0; i < nsegs; i++)
        {
            struct svg_pt p0 = pts[i];
            struct svg_pt p1 = pts[(i + 1) % n];
            float dx = p1.x - p0.x, dy = p1.y - p0.y;
            float len = hypotf(dx, dy);
            if (len > 1e-5f)
            {
                nx[i] = -dy / len;
                ny[i] =  dx / len;
                any_valid = true;
            }
            else
            {
                nx[i] = i > 0 ? nx[i - 1] : 0.0f;
                ny[i] = i > 0 ? ny[i - 1] : 1.0f;
            }
        }
        if (!any_valid) { free(nx); free(ny); continue; }

        if (sub.closed)
        {
            float mx = 0.5f * (pts[0].x + pts[1].x);
            float my = 0.5f * (pts[0].y + pts[1].y);

            /* Outer contour (+n) */
            path_begin_sub(out, mx + nx[0] * hw, my + ny[0] * hw);
            for (int i = 1; i < n; i++)
                add_join(out, pts[i], nx[i - 1], ny[i - 1],
                         nx[i], ny[i], hw, join, miter_limit);
            add_join(out, pts[0], nx[nsegs - 1], ny[nsegs - 1],
                     nx[0], ny[0], hw, join, miter_limit);
            path_add_pt(out, mx + nx[0] * hw, my + ny[0] * hw);
            path_close_sub(out);

            /* Inner contour (-n, opposite winding cuts out the interior) */
            path_begin_sub(out, mx - nx[0] * hw, my - ny[0] * hw);
            add_join(out, pts[0], -nx[0], -ny[0],
                     -nx[nsegs - 1], -ny[nsegs - 1], hw, join, miter_limit);
            for (int i = n - 1; i >= 1; i--)
                add_join(out, pts[i], -nx[i], -ny[i],
                         -nx[i - 1], -ny[i - 1], hw, join, miter_limit);
            path_add_pt(out, mx - nx[0] * hw, my - ny[0] * hw);
            path_close_sub(out);
        }
        else
        {
            /* Forward side */
            float sx = pts[0].x, sy = pts[0].y;
            float tx = ny[0], ty = -nx[0]; /* unit tangent p0 -> p1 */
            if (cap == CAP_SQUARE)
                path_begin_sub(out, sx - tx * hw + nx[0] * hw,
                                    sy - ty * hw + ny[0] * hw);
            else
                path_begin_sub(out, sx + nx[0] * hw, sy + ny[0] * hw);

            for (int i = 1; i < n - 1; i++)
                add_join(out, pts[i], nx[i - 1], ny[i - 1],
                         nx[i], ny[i], hw, join, miter_limit);

            /* End cap at pts[n - 1] */
            struct svg_pt pe = pts[n - 1];
            float ex = ny[nsegs - 1], ey = -nx[nsegs - 1];
            if (cap == CAP_SQUARE)
            {
                path_add_pt(out, pe.x + ex * hw + nx[nsegs - 1] * hw,
                                 pe.y + ey * hw + ny[nsegs - 1] * hw);
                path_add_pt(out, pe.x + ex * hw - nx[nsegs - 1] * hw,
                                 pe.y + ey * hw - ny[nsegs - 1] * hw);
            }
            else if (cap == CAP_ROUND)
            {
                path_add_pt(out, pe.x + nx[nsegs - 1] * hw,
                                 pe.y + ny[nsegs - 1] * hw);
                float a0 = atan2f(ny[nsegs - 1], nx[nsegs - 1]);
                add_arc_fan(out, pe.x, pe.y, a0, a0 - (float)M_PI, hw);
            }
            else
            {
                path_add_pt(out, pe.x + nx[nsegs - 1] * hw,
                                 pe.y + ny[nsegs - 1] * hw);
                path_add_pt(out, pe.x - nx[nsegs - 1] * hw,
                                 pe.y - ny[nsegs - 1] * hw);
            }

            /* Backward side */
            for (int i = n - 2; i >= 1; i--)
                add_join(out, pts[i], -nx[i], -ny[i],
                         -nx[i - 1], -ny[i - 1], hw, join, miter_limit);

            /* Start cap at pts[0] */
            if (cap == CAP_SQUARE)
            {
                path_add_pt(out, sx - tx * hw - nx[0] * hw,
                                 sy - ty * hw - ny[0] * hw);
            }
            else if (cap == CAP_ROUND)
            {
                path_add_pt(out, sx - nx[0] * hw, sy - ny[0] * hw);
                float a0 = atan2f(-ny[0], -nx[0]);
                add_arc_fan(out, sx, sy, a0, a0 - (float)M_PI, hw);
            }
            else
            {
                path_add_pt(out, sx - nx[0] * hw, sy - ny[0] * hw);
            }
            path_close_sub(out);
        }

        free(nx);
        free(ny);
    }
}

/* ---- 8x8 supersampled scanline rasterizer ----------------------------- */

struct svg_edge { float x; int dir; };

static int cmp_edge(const void *a, const void *b)
{
    float xa = ((const struct svg_edge *)a)->x;
    float xb = ((const struct svg_edge *)b)->x;
    return (xa > xb) - (xa < xb);
}

static void blend_pixel(uint8_t *px, struct svg_color c, uint32_t cov, uint32_t max_cov)
{
    if (cov == 0 || c.a == 0)
        return;
    uint32_t sa = (c.a * cov + max_cov / 2) / max_cov;
    if (sa == 0)
        return;
    uint32_t da = px[3];
    uint32_t inv_sa = 255 - sa;
    uint32_t out_a = sa + (da * inv_sa + 127) / 255;
    if (out_a == 0)
        return;
    /* Premultiplied-equivalent Porter-Duff source-over */
    uint32_t r = (c.r * sa * 255 + px[0] * da * inv_sa) / (out_a * 255);
    uint32_t g = (c.g * sa * 255 + px[1] * da * inv_sa) / (out_a * 255);
    uint32_t b = (c.b * sa * 255 + px[2] * da * inv_sa) / (out_a * 255);
    px[0] = (uint8_t)(r > 255 ? 255 : r);
    px[1] = (uint8_t)(g > 255 ? 255 : g);
    px[2] = (uint8_t)(b > 255 ? 255 : b);
    px[3] = (uint8_t)(out_a > 255 ? 255 : out_a);
}

static void rasterize_fill(uint8_t *rgba, int w, int h,
                           const struct svg_path_buf *pb,
                           struct svg_color color, enum svg_fill_rule rule)
{
    if (pb->nsubs == 0 || color.a == 0)
        return;

    /* Bounding box + max edge count */
    float min_y = (float)h, max_y = 0.0f;
    int max_edges = 0;
    for (int s = 0; s < pb->nsubs; s++)
    {
        struct svg_sub sub = pb->subs[s];
        if (sub.count < 2) continue;
        max_edges += sub.count;
        for (int i = 0; i < sub.count; i++)
        {
            float py = pb->pts[sub.start + i].y;
            if (py < min_y) min_y = py;
            if (py > max_y) max_y = py;
        }
    }
    int y0 = (int)floorf(min_y);
    int y1 = (int)ceilf(max_y);
    if (y0 < 0) y0 = 0;
    if (y1 > h) y1 = h;
    if (y0 >= y1 || max_edges == 0)
        return;

    struct svg_edge *edges = malloc((size_t)max_edges * sizeof(*edges));
    uint16_t *cov = malloc((size_t)w * sizeof(*cov));
    if (!edges || !cov) { free(edges); free(cov); return; }

    for (int y = y0; y < y1; y++)
    {
        memset(cov, 0, (size_t)w * sizeof(*cov));
        for (int sy = 0; sy < SVG_SS; sy++)
        {
            float scan_y = (float)y + ((float)sy + 0.5f) * (1.0f / (float)SVG_SS);
            int nedges = 0;

            for (int s = 0; s < pb->nsubs; s++)
            {
                struct svg_sub sub = pb->subs[s];
                if (sub.count < 2) continue;
                const struct svg_pt *pts = pb->pts + sub.start;
                for (int i = 0; i < sub.count; i++)
                {
                    struct svg_pt p0 = pts[i];
                    struct svg_pt p1 = pts[(i + 1) % sub.count];
                    if ((p0.y <= scan_y && p1.y > scan_y) ||
                        (p1.y <= scan_y && p0.y > scan_y))
                    {
                        int dir = (p1.y > p0.y) ? 1 : -1;
                        float t = (scan_y - p0.y) / (p1.y - p0.y);
                        edges[nedges++] = (struct svg_edge){
                            p0.x + t * (p1.x - p0.x), dir
                        };
                    }
                }
            }
            if (nedges < 2)
                continue;

            qsort(edges, (size_t)nedges, sizeof(*edges), cmp_edge);

            int winding = 0;
            float span_start = 0.0f;
            bool inside = false;
            for (int e = 0; e < nedges; e++)
            {
                winding += edges[e].dir;
                bool now_in = (rule == RULE_EVENODD)
                                  ? ((winding & 1) != 0)
                                  : (winding != 0);
                if (!inside && now_in)
                {
                    span_start = edges[e].x;
                    inside = true;
                }
                else if (inside && !now_in)
                {
                    float sx0 = span_start * (float)SVG_SS;
                    float sx1 = edges[e].x * (float)SVG_SS;
                    int ix0 = (int)floorf(sx0 + 0.5f);
                    int ix1 = (int)floorf(sx1 + 0.5f);
                    if (ix0 < 0) ix0 = 0;
                    if (ix1 > w * SVG_SS) ix1 = w * SVG_SS;
                    for (int sx = ix0; sx < ix1; sx++)
                        cov[sx / SVG_SS]++;
                    inside = false;
                }
            }
        }

        uint8_t *row = rgba + (size_t)y * (size_t)w * 4;
        for (int x = 0; x < w; x++)
            if (cov[x])
                blend_pixel(row + x * 4, color, cov[x], SVG_SS * SVG_SS);
    }

    free(edges);
    free(cov);
}

/* ---- SVG attribute & path parsers ------------------------------------- */

static const char *skip_ws_comma(const char *p)
{
    while (*p && (isspace((unsigned char)*p) || *p == ','))
        p++;
    return p;
}

static bool parse_num(const char **pp, float *out)
{
    const char *p = skip_ws_comma(*pp);
    if (!*p) return false;
    char *end = NULL;
    float v = strtof(p, &end);
    if (end == p) return false;
    *out = v;
    *pp = end;
    return true;
}

/* SVG arc flags can be concatenated without spaces, e.g. "01" -> 0, 1 */
static bool parse_arc_flag(const char **pp, int *out)
{
    const char *p = skip_ws_comma(*pp);
    if (*p != '0' && *p != '1')
        return false;
    *out = (*p - '0');
    *pp = p + 1;
    return true;
}

static int hex_val(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static void parse_paint(const char *val, enum svg_paint_kind *kind,
                        struct svg_color *col)
{
    while (*val && isspace((unsigned char)*val)) val++;
    if (!*val) return;

    if (!strncasecmp(val, "none", 4) || !strncasecmp(val, "transparent", 11))
    {
        *kind = PAINT_NONE;
        return;
    }
    if (!strncasecmp(val, "currentColor", 12) || !strncasecmp(val, "url(", 4))
    {
        *kind = PAINT_COLOR;
        *col = (struct svg_color){ 0, 0, 0, 255 };
        return;
    }
    if (*val == '#')
    {
        val++;
        int n = 0;
        while (hex_val(val[n]) >= 0) n++;
        if (n == 3 || n == 4)
        {
            int r = hex_val(val[0]) * 17;
            int g = hex_val(val[1]) * 17;
            int b = hex_val(val[2]) * 17;
            int a = (n == 4) ? hex_val(val[3]) * 17 : 255;
            *kind = PAINT_COLOR;
            *col = (struct svg_color){ (uint8_t)r, (uint8_t)g, (uint8_t)b, (uint8_t)a };
            return;
        }
        if (n >= 6)
        {
            int r = (hex_val(val[0]) << 4) | hex_val(val[1]);
            int g = (hex_val(val[2]) << 4) | hex_val(val[3]);
            int b = (hex_val(val[4]) << 4) | hex_val(val[5]);
            int a = (n >= 8) ? ((hex_val(val[6]) << 4) | hex_val(val[7])) : 255;
            *kind = PAINT_COLOR;
            *col = (struct svg_color){ (uint8_t)r, (uint8_t)g, (uint8_t)b, (uint8_t)a };
            return;
        }
    }
    if (!strncasecmp(val, "rgb", 3))
    {
        const char *p = strchr(val, '(');
        if (p)
        {
            p++;
            float r = 0, g = 0, b = 0, a = 1.0f;
            parse_num(&p, &r);
            parse_num(&p, &g);
            parse_num(&p, &b);
            if (!parse_num(&p, &a)) a = 1.0f;
            if (r < 0) r = 0; else if (r > 255) r = 255;
            if (g < 0) g = 0; else if (g > 255) g = 255;
            if (b < 0) b = 0; else if (b > 255) b = 255;
            if (a < 0) a = 0; else if (a > 1.0f) a = 1.0f;
            *kind = PAINT_COLOR;
            *col = (struct svg_color){
                (uint8_t)r, (uint8_t)g, (uint8_t)b, (uint8_t)(a * 255.0f + 0.5f)
            };
            return;
        }
    }
    /* Named colours */
    static const struct { const char *name; uint8_t r, g, b; } named[] = {
        { "black",   0,   0,   0   },
        { "white",   255, 255, 255 },
        { "gray",    128, 128, 128 },
        { "grey",    128, 128, 128 },
        { "silver",  192, 192, 192 },
        { "red",     255, 0,   0   },
        { "green",   0,   128, 0   },
        { "blue",    0,   0,   255 },
        { "yellow",  255, 255, 0   },
        { "orange",  255, 165, 0   },
    };
    for (size_t i = 0; i < sizeof(named) / sizeof(named[0]); i++)
    {
        size_t l = strlen(named[i].name);
        if (!strncasecmp(val, named[i].name, l) &&
            (!val[l] || isspace((unsigned char)val[l]) || val[l] == ';'))
        {
            *kind = PAINT_COLOR;
            *col = (struct svg_color){ named[i].r, named[i].g, named[i].b, 255 };
            return;
        }
    }
}

static struct svg_mat parse_transform(const char *s)
{
    struct svg_mat m = mat_identity();
    while (*s)
    {
        s = skip_ws_comma(s);
        if (!*s) break;
        const char *lp = strchr(s, '(');
        if (!lp) break;
        size_t namelen = (size_t)(lp - s);
        while (namelen > 0 && isspace((unsigned char)s[namelen - 1]))
            namelen--;

        const char *p = lp + 1;
        float v[6] = { 0 };
        int nv = 0;
        while (nv < 6)
        {
            p = skip_ws_comma(p);
            if (!*p || *p == ')') break;
            if (!parse_num(&p, &v[nv])) break;
            nv++;
        }
        const char *rp = strchr(p, ')');
        s = rp ? rp + 1 : p;

        struct svg_mat t = mat_identity();
        if (namelen == 6 && !strncasecmp(lp - namelen, "matrix", 6) && nv == 6)
        {
            t = (struct svg_mat){ v[0], v[1], v[2], v[3], v[4], v[5] };
        }
        else if (namelen == 9 && !strncasecmp(lp - namelen, "translate", 9) && nv >= 1)
        {
            t.e = v[0];
            t.f = (nv >= 2) ? v[1] : 0.0f;
        }
        else if (namelen == 5 && !strncasecmp(lp - namelen, "scale", 5) && nv >= 1)
        {
            t.a = v[0];
            t.d = (nv >= 2) ? v[1] : v[0];
        }
        else if (namelen == 6 && !strncasecmp(lp - namelen, "rotate", 6) && nv >= 1)
        {
            float rad = v[0] * (float)(M_PI / 180.0);
            float cs = cosf(rad), sn = sinf(rad);
            if (nv >= 3)
            {
                t = mat_mul((struct svg_mat){ 1, 0, 0, 1, v[1], v[2] },
                    mat_mul((struct svg_mat){ cs, sn, -sn, cs, 0, 0 },
                            (struct svg_mat){ 1, 0, 0, 1, -v[1], -v[2] }));
            }
            else
                t = (struct svg_mat){ cs, sn, -sn, cs, 0, 0 };
        }
        else if (namelen == 5 && !strncasecmp(lp - namelen, "skewX", 5) && nv >= 1)
        {
            t.c = tanf(v[0] * (float)(M_PI / 180.0));
        }
        else if (namelen == 5 && !strncasecmp(lp - namelen, "skewY", 5) && nv >= 1)
        {
            t.b = tanf(v[0] * (float)(M_PI / 180.0));
        }
        m = mat_mul(m, t);
    }
    return m;
}

static void apply_style_kv(struct svg_style *st, const char *k, size_t klen,
                           const char *v)
{
    if (klen == 4 && !strncasecmp(k, "fill", 4))
        parse_paint(v, &st->fill_kind, &st->fill);
    else if (klen == 6 && !strncasecmp(k, "stroke", 6))
        parse_paint(v, &st->stroke_kind, &st->stroke);
    else if (klen == 12 && !strncasecmp(k, "stroke-width", 12))
    {
        float f;
        if (parse_num(&v, &f) && f >= 0.0f) st->stroke_width = f;
    }
    else if (klen == 9 && !strncasecmp(k, "fill-rule", 9))
    {
        while (*v && isspace((unsigned char)*v)) v++;
        st->fill_rule = !strncasecmp(v, "evenodd", 7) ? RULE_EVENODD : RULE_NONZERO;
    }
    else if (klen == 7 && !strncasecmp(k, "opacity", 7))
    {
        float f;
        if (parse_num(&v, &f)) st->opacity *= f;
    }
    else if (klen == 12 && !strncasecmp(k, "fill-opacity", 12))
    {
        float f;
        if (parse_num(&v, &f)) st->fill_opacity = f;
    }
    else if (klen == 14 && !strncasecmp(k, "stroke-opacity", 14))
    {
        float f;
        if (parse_num(&v, &f)) st->stroke_opacity = f;
    }
    else if (klen == 14 && !strncasecmp(k, "stroke-linecap", 14))
    {
        while (*v && isspace((unsigned char)*v)) v++;
        if (!strncasecmp(v, "round", 5))       st->linecap = CAP_ROUND;
        else if (!strncasecmp(v, "square", 6)) st->linecap = CAP_SQUARE;
        else                                   st->linecap = CAP_BUTT;
    }
    else if (klen == 15 && !strncasecmp(k, "stroke-linejoin", 15))
    {
        while (*v && isspace((unsigned char)*v)) v++;
        if (!strncasecmp(v, "round", 5))      st->linejoin = JOIN_ROUND;
        else if (!strncasecmp(v, "bevel", 5)) st->linejoin = JOIN_BEVEL;
        else                                  st->linejoin = JOIN_MITER;
    }
    else if (klen == 17 && !strncasecmp(k, "stroke-miterlimit", 17))
    {
        float f;
        if (parse_num(&v, &f) && f >= 1.0f) st->miter_limit = f;
    }
    else if (klen == 9 && !strncasecmp(k, "transform", 9))
    {
        st->xform = mat_mul(st->xform, parse_transform(v));
    }
    else if (klen == 5 && !strncasecmp(k, "style", 5))
    {
        const char *p = v;
        while (*p)
        {
            while (*p && (isspace((unsigned char)*p) || *p == ';')) p++;
            if (!*p) break;
            const char *colon = strchr(p, ':');
            if (!colon) break;
            size_t sklen = (size_t)(colon - p);
            while (sklen > 0 && isspace((unsigned char)p[sklen - 1])) sklen--;
            const char *sv = colon + 1;
            const char *semi = strchr(sv, ';');
            char valbuf[128];
            size_t svlen = semi ? (size_t)(semi - sv) : strlen(sv);
            if (svlen >= sizeof(valbuf)) svlen = sizeof(valbuf) - 1;
            memcpy(valbuf, sv, svlen);
            valbuf[svlen] = '\0';
            apply_style_kv(st, p, sklen, valbuf);
            p = semi ? semi + 1 : sv + strlen(sv);
        }
    }
}

static void parse_svg_path_d(const char *d, struct svg_mat m,
                             struct svg_path_buf *pb)
{
    path_clear(pb);
    const char *p = d;
    char cmd = 0, prev_cmd = 0;
    float cx = 0.0f, cy = 0.0f;
    float sx = 0.0f, sy = 0.0f;
    float ctrl_x = 0.0f, ctrl_y = 0.0f;

    while (*p)
    {
        p = skip_ws_comma(p);
        if (!*p) break;
        if (isalpha((unsigned char)*p))
            cmd = *p++;
        else if (!cmd)
            break;

        bool rel = (cmd >= 'a' && cmd <= 'z');
        char u = (char)toupper((unsigned char)cmd);

        switch (u)
        {
            case 'M':
            {
                float x, y;
                if (!parse_num(&p, &x) || !parse_num(&p, &y)) return;
                if (rel) { x += cx; y += cy; }
                cx = sx = x; cy = sy = y;
                float tx, ty;
                mat_point(m, cx, cy, &tx, &ty);
                path_begin_sub(pb, tx, ty);
                cmd = rel ? 'l' : 'L';
                break;
            }
            case 'L':
            {
                float x, y;
                if (!parse_num(&p, &x) || !parse_num(&p, &y)) return;
                if (rel) { x += cx; y += cy; }
                cx = x; cy = y;
                float tx, ty;
                mat_point(m, cx, cy, &tx, &ty);
                path_add_pt(pb, tx, ty);
                break;
            }
            case 'H':
            {
                float x;
                if (!parse_num(&p, &x)) return;
                if (rel) x += cx;
                cx = x;
                float tx, ty;
                mat_point(m, cx, cy, &tx, &ty);
                path_add_pt(pb, tx, ty);
                break;
            }
            case 'V':
            {
                float y;
                if (!parse_num(&p, &y)) return;
                if (rel) y += cy;
                cy = y;
                float tx, ty;
                mat_point(m, cx, cy, &tx, &ty);
                path_add_pt(pb, tx, ty);
                break;
            }
            case 'C':
            {
                float x1, y1, x2, y2, x, y;
                if (!parse_num(&p, &x1) || !parse_num(&p, &y1) ||
                    !parse_num(&p, &x2) || !parse_num(&p, &y2) ||
                    !parse_num(&p, &x)  || !parse_num(&p, &y))
                    return;
                if (rel) { x1 += cx; y1 += cy; x2 += cx; y2 += cy; x += cx; y += cy; }
                float p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y;
                mat_point(m, cx, cy, &p0x, &p0y);
                mat_point(m, x1, y1, &p1x, &p1y);
                mat_point(m, x2, y2, &p2x, &p2y);
                mat_point(m, x,  y,  &p3x, &p3y);
                flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);
                ctrl_x = x2; ctrl_y = y2;
                cx = x; cy = y;
                break;
            }
            case 'S':
            {
                float x2, y2, x, y;
                if (!parse_num(&p, &x2) || !parse_num(&p, &y2) ||
                    !parse_num(&p, &x)  || !parse_num(&p, &y))
                    return;
                if (rel) { x2 += cx; y2 += cy; x += cx; y += cy; }
                float prev_u = (char)toupper((unsigned char)prev_cmd);
                float x1 = (prev_u == 'C' || prev_u == 'S') ? (2.0f * cx - ctrl_x) : cx;
                float y1 = (prev_u == 'C' || prev_u == 'S') ? (2.0f * cy - ctrl_y) : cy;
                float p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y;
                mat_point(m, cx, cy, &p0x, &p0y);
                mat_point(m, x1, y1, &p1x, &p1y);
                mat_point(m, x2, y2, &p2x, &p2y);
                mat_point(m, x,  y,  &p3x, &p3y);
                flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);
                ctrl_x = x2; ctrl_y = y2;
                cx = x; cy = y;
                break;
            }
            case 'Q':
            {
                float x1, y1, x, y;
                if (!parse_num(&p, &x1) || !parse_num(&p, &y1) ||
                    !parse_num(&p, &x)  || !parse_num(&p, &y))
                    return;
                if (rel) { x1 += cx; y1 += cy; x += cx; y += cy; }
                float p0x, p0y, p1x, p1y, p2x, p2y;
                mat_point(m, cx, cy, &p0x, &p0y);
                mat_point(m, x1, y1, &p1x, &p1y);
                mat_point(m, x,  y,  &p2x, &p2y);
                flatten_quad(pb, p0x, p0y, p1x, p1y, p2x, p2y);
                ctrl_x = x1; ctrl_y = y1;
                cx = x; cy = y;
                break;
            }
            case 'T':
            {
                float x, y;
                if (!parse_num(&p, &x) || !parse_num(&p, &y))
                    return;
                if (rel) { x += cx; y += cy; }
                float prev_u = (char)toupper((unsigned char)prev_cmd);
                float x1 = (prev_u == 'Q' || prev_u == 'T') ? (2.0f * cx - ctrl_x) : cx;
                float y1 = (prev_u == 'Q' || prev_u == 'T') ? (2.0f * cy - ctrl_y) : cy;
                float p0x, p0y, p1x, p1y, p2x, p2y;
                mat_point(m, cx, cy, &p0x, &p0y);
                mat_point(m, x1, y1, &p1x, &p1y);
                mat_point(m, x,  y,  &p2x, &p2y);
                flatten_quad(pb, p0x, p0y, p1x, p1y, p2x, p2y);
                ctrl_x = x1; ctrl_y = y1;
                cx = x; cy = y;
                break;
            }
            case 'A':
            {
                float rx, ry, rot, x, y;
                int large_arc, sweep;
                if (!parse_num(&p, &rx) || !parse_num(&p, &ry) ||
                    !parse_num(&p, &rot) ||
                    !parse_arc_flag(&p, &large_arc) ||
                    !parse_arc_flag(&p, &sweep) ||
                    !parse_num(&p, &x) || !parse_num(&p, &y))
                    return;
                if (rel) { x += cx; y += cy; }
                flatten_arc(pb, m, cx, cy, rx, ry, rot, large_arc, sweep, x, y);
                cx = x; cy = y;
                break;
            }
            case 'Z':
            {
                float tx, ty;
                mat_point(m, sx, sy, &tx, &ty);
                path_add_pt(pb, tx, ty);
                path_close_sub(pb);
                cx = sx; cy = sy;
                cmd = 0;
                break;
            }
            default:
                return;
        }
        prev_cmd = u;
    }
}

static void build_rect(struct svg_path_buf *pb, struct svg_mat m,
                       float x, float y, float w, float h, float rx, float ry)
{
    path_clear(pb);
    if (w <= 0.0f || h <= 0.0f) return;
    if (rx <= 0.0f && ry > 0.0f) rx = ry;
    if (ry <= 0.0f && rx > 0.0f) ry = rx;
    if (rx > 0.5f * w) rx = 0.5f * w;
    if (ry > 0.5f * h) ry = 0.5f * h;

    float tx, ty;
    if (rx <= 0.0f || ry <= 0.0f)
    {
        mat_point(m, x,     y,     &tx, &ty); path_begin_sub(pb, tx, ty);
        mat_point(m, x + w, y,     &tx, &ty); path_add_pt(pb, tx, ty);
        mat_point(m, x + w, y + h, &tx, &ty); path_add_pt(pb, tx, ty);
        mat_point(m, x,     y + h, &tx, &ty); path_add_pt(pb, tx, ty);
        mat_point(m, x,     y,     &tx, &ty); path_add_pt(pb, tx, ty);
        path_close_sub(pb);
        return;
    }
    const float k = 0.5522847498f;
    float kx = rx * k, ky = ry * k;
    float p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y;
    mat_point(m, x + rx, y, &tx, &ty);
    path_begin_sub(pb, tx, ty);
    mat_point(m, x + w - rx, y, &tx, &ty);
    path_add_pt(pb, tx, ty);

    mat_point(m, x + w - rx,      y,          &p0x, &p0y);
    mat_point(m, x + w - rx + kx, y,          &p1x, &p1y);
    mat_point(m, x + w,           y + ry - ky,&p2x, &p2y);
    mat_point(m, x + w,           y + ry,     &p3x, &p3y);
    flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);

    mat_point(m, x + w, y + h - ry, &tx, &ty);
    path_add_pt(pb, tx, ty);

    mat_point(m, x + w,           y + h - ry,     &p0x, &p0y);
    mat_point(m, x + w,           y + h - ry + ky,&p1x, &p1y);
    mat_point(m, x + w - rx + kx, y + h,          &p2x, &p2y);
    mat_point(m, x + w - rx,      y + h,          &p3x, &p3y);
    flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);

    mat_point(m, x + rx, y + h, &tx, &ty);
    path_add_pt(pb, tx, ty);

    mat_point(m, x + rx,      y + h,          &p0x, &p0y);
    mat_point(m, x + rx - kx, y + h,          &p1x, &p1y);
    mat_point(m, x,           y + h - ry + ky,&p2x, &p2y);
    mat_point(m, x,           y + h - ry,     &p3x, &p3y);
    flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);

    mat_point(m, x, y + ry, &tx, &ty);
    path_add_pt(pb, tx, ty);

    mat_point(m, x,           y + ry,     &p0x, &p0y);
    mat_point(m, x,           y + ry - ky,&p1x, &p1y);
    mat_point(m, x + rx - kx, y,          &p2x, &p2y);
    mat_point(m, x + rx,      y,          &p3x, &p3y);
    flatten_cubic(pb, p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y, 0);
    path_close_sub(pb);
}

static void build_ellipse(struct svg_path_buf *pb, struct svg_mat m,
                          float cx, float cy, float rx, float ry)
{
    path_clear(pb);
    if (rx <= 0.0f || ry <= 0.0f) return;
    const float k = 0.5522847498f;
    float kx = rx * k, ky = ry * k;
    float q[13][2] = {
        { cx + rx, cy },
        { cx + rx, cy + ky }, { cx + kx, cy + ry }, { cx, cy + ry },
        { cx - kx, cy + ry }, { cx - rx, cy + ky }, { cx - rx, cy },
        { cx - rx, cy - ky }, { cx - kx, cy - ry }, { cx, cy - ry },
        { cx + kx, cy - ry }, { cx + rx, cy - ky }, { cx + rx, cy },
    };
    float tx[13], ty[13];
    for (int i = 0; i < 13; i++)
        mat_point(m, q[i][0], q[i][1], &tx[i], &ty[i]);
    path_begin_sub(pb, tx[0], ty[0]);
    for (int i = 0; i < 12; i += 3)
        flatten_cubic(pb, tx[i], ty[i], tx[i + 1], ty[i + 1],
                      tx[i + 2], ty[i + 2], tx[i + 3], ty[i + 3], 0);
    path_close_sub(pb);
}

static void draw_shape(uint8_t *rgba, int w, int h,
                       const struct svg_style *st,
                       const struct svg_path_buf *pb,
                       struct svg_path_buf *stroke_tmp)
{
    if (pb->nsubs == 0)
        return;

    if (st->fill_kind == PAINT_COLOR)
    {
        float a = (float)st->fill.a * st->fill_opacity * st->opacity;
        if (a > 0.5f)
        {
            struct svg_color fc = st->fill;
            fc.a = (uint8_t)(a > 255.0f ? 255 : (a + 0.5f));
            rasterize_fill(rgba, w, h, pb, fc, st->fill_rule);
        }
    }

    if (st->stroke_kind == PAINT_COLOR && st->stroke_width > 0.0f)
    {
        float a = (float)st->stroke.a * st->stroke_opacity * st->opacity;
        float sw = st->stroke_width * mat_scale_factor(st->xform);
        if (a > 0.5f && sw > 0.05f)
        {
            struct svg_color sc = st->stroke;
            sc.a = (uint8_t)(a > 255.0f ? 255 : (a + 0.5f));
            expand_stroke(pb, sw, st->linecap, st->linejoin,
                          st->miter_limit, stroke_tmp);
            rasterize_fill(rgba, w, h, stroke_tmp, sc, RULE_NONZERO);
        }
    }
}

/* ---- XML / SVG walker -------------------------------------------------- */

static bool is_skip_container(const char *tag, size_t len)
{
    static const char *const skip_tags[] = {
        "defs", "clipPath", "filter", "mask", "pattern",
        "linearGradient", "radialGradient", "symbol", "marker",
        "style", "script", "title", "desc", "metadata", "text"
    };
    for (size_t i = 0; i < sizeof(skip_tags) / sizeof(skip_tags[0]); i++)
        if (strlen(skip_tags[i]) == len && !strncasecmp(tag, skip_tags[i], len))
            return true;
    return false;
}

static bool rasterize_svg_text(const char *xml, int icon_w, int icon_h,
                               unsigned char *out_alpha4)
{
    uint8_t *rgba = calloc((size_t)(icon_w * icon_h), 4);
    char *d_buf = NULL;
    size_t d_cap = 0;
    if (!rgba)
        return false;

    struct svg_style stack[SVG_MAX_STACK];
    int sp = 0;
    stack[0] = style_default();

    struct svg_path_buf pb, stroke_pb;
    path_init(&pb);
    path_init(&stroke_pb);

    int skip_depth = 0;
    bool root_seen = false;
    const char *p = xml;

    while (*p)
    {
        const char *lt = strchr(p, '<');
        if (!lt) break;
        p = lt + 1;

        /* Comments, XML prolog, DOCTYPE, CDATA */
        if (!strncmp(p, "!--", 3))
        {
            const char *end = strstr(p + 3, "-->");
            p = end ? end + 3 : p + strlen(p);
            continue;
        }
        if (*p == '?' || *p == '!')
        {
            const char *gt = strchr(p, '>');
            p = gt ? gt + 1 : p + strlen(p);
            continue;
        }

        /* Closing tag </...> */
        if (*p == '/')
        {
            p++;
            const char *tstart = p;
            while (*p && !isspace((unsigned char)*p) && *p != '>') p++;
            size_t tlen = (size_t)(p - tstart);
            const char *gt = strchr(p, '>');
            p = gt ? gt + 1 : p;

            if (skip_depth > 0)
            {
                if (is_skip_container(tstart, tlen))
                    skip_depth--;
            }
            else if (sp > 0 && ((tlen == 1 && !strncasecmp(tstart, "g", 1)) ||
                                (tlen == 3 && !strncasecmp(tstart, "svg", 3)) ||
                                (tlen == 1 && !strncasecmp(tstart, "a", 1))))
            {
                sp--;
            }
            continue;
        }

        /* Opening / self-closing tag */
        const char *tstart = p;
        while (*p && !isspace((unsigned char)*p) && *p != '/' && *p != '>') p++;
        size_t tlen = (size_t)(p - tstart);

        /* Scan to end of tag (respecting quotes) to check self-closing */
        const char *attr_p = p;
        char quote = 0;
        while (*p && (quote || *p != '>'))
        {
            if (quote) { if (*p == quote) quote = 0; }
            else if (*p == '"' || *p == '\'') quote = *p;
            p++;
        }
        const char *tag_end = p;
        bool self_closing = (tag_end > attr_p && tag_end[-1] == '/');
        if (*p == '>') p++;

        if (skip_depth > 0)
        {
            if (!self_closing && is_skip_container(tstart, tlen))
                skip_depth++;
            continue;
        }
        if (is_skip_container(tstart, tlen))
        {
            if (!self_closing)
                skip_depth = 1;
            continue;
        }

        struct svg_style cur = stack[sp];
        float vb_x = 0, vb_y = 0, vb_w = 0, vb_h = 0;
        bool have_vb = false;
        float svg_w = 0, svg_h = 0;
        float sh_x = 0, sh_y = 0, sh_w = 0, sh_h = 0, sh_rx = 0, sh_ry = 0;
        float sh_cx = 0, sh_cy = 0, sh_r = 0;
        float sh_x1 = 0, sh_y1 = 0, sh_x2 = 0, sh_y2 = 0;
        const char *d_start = NULL;
        size_t d_len = 0;

        /* Parse attributes */
        const char *ap = attr_p;
        const char *aend = self_closing ? (tag_end - 1) : tag_end;
        while (ap < aend)
        {
            while (ap < aend && isspace((unsigned char)*ap)) ap++;
            if (ap >= aend || *ap == '/') break;
            const char *kstart = ap;
            while (ap < aend && !isspace((unsigned char)*ap) && *ap != '=' && *ap != '/')
                ap++;
            size_t klen = (size_t)(ap - kstart);
            while (ap < aend && isspace((unsigned char)*ap)) ap++;
            if (ap >= aend || *ap != '=') continue;
            ap++;
            while (ap < aend && isspace((unsigned char)*ap)) ap++;
            if (ap >= aend) break;

            const char *vstart;
            size_t vlen = 0;
            if (*ap == '"' || *ap == '\'')
            {
                char q = *ap++;
                vstart = ap;
                while (ap < aend && *ap != q) ap++;
                vlen = (size_t)(ap - vstart);
                if (ap < aend && *ap == q) ap++;
            }
            else
            {
                vstart = ap;
                while (ap < aend && !isspace((unsigned char)*ap)) ap++;
                vlen = (size_t)(ap - vstart);
            }

            if ((klen == 1 && (*kstart == 'd' || *kstart == 'D')) ||
                (klen == 6 && !strncasecmp(kstart, "points", 6)))
            {
                d_start = vstart;
                d_len = vlen;
                continue;
            }

            char vbuf[256];
            size_t clen = vlen < sizeof(vbuf) - 1 ? vlen : sizeof(vbuf) - 1;
            memcpy(vbuf, vstart, clen);
            vbuf[clen] = '\0';
            const char *vp = vbuf;

            if (tlen == 3 && !strncasecmp(tstart, "svg", 3))
            {
                if (klen == 7 && !strncasecmp(kstart, "viewBox", 7))
                {
                    if (parse_num(&vp, &vb_x) && parse_num(&vp, &vb_y) &&
                        parse_num(&vp, &vb_w) && parse_num(&vp, &vb_h) &&
                        vb_w > 0.0f && vb_h > 0.0f)
                        have_vb = true;
                    continue;
                }
                if (klen == 5 && !strncasecmp(kstart, "width", 5))
                    { parse_num(&vp, &svg_w); continue; }
                if (klen == 6 && !strncasecmp(kstart, "height", 6))
                    { parse_num(&vp, &svg_h); continue; }
            }

            if (klen == 1 && *kstart == 'x')      { parse_num(&vp, &sh_x);  continue; }
            if (klen == 1 && *kstart == 'y')      { parse_num(&vp, &sh_y);  continue; }
            if (klen == 5 && !strncasecmp(kstart, "width", 5))  { parse_num(&vp, &sh_w);  continue; }
            if (klen == 6 && !strncasecmp(kstart, "height", 6)) { parse_num(&vp, &sh_h);  continue; }
            if (klen == 2 && !strncasecmp(kstart, "rx", 2))     { parse_num(&vp, &sh_rx); continue; }
            if (klen == 2 && !strncasecmp(kstart, "ry", 2))     { parse_num(&vp, &sh_ry); continue; }
            if (klen == 2 && !strncasecmp(kstart, "cx", 2))     { parse_num(&vp, &sh_cx); continue; }
            if (klen == 2 && !strncasecmp(kstart, "cy", 2))     { parse_num(&vp, &sh_cy); continue; }
            if (klen == 1 && *kstart == 'r')                    { parse_num(&vp, &sh_r);  continue; }
            if (klen == 2 && !strncasecmp(kstart, "x1", 2))     { parse_num(&vp, &sh_x1); continue; }
            if (klen == 2 && !strncasecmp(kstart, "y1", 2))     { parse_num(&vp, &sh_y1); continue; }
            if (klen == 2 && !strncasecmp(kstart, "x2", 2))     { parse_num(&vp, &sh_x2); continue; }
            if (klen == 2 && !strncasecmp(kstart, "y2", 2))     { parse_num(&vp, &sh_y2); continue; }

            apply_style_kv(&cur, kstart, klen, vbuf);
        }

        if (tlen == 3 && !strncasecmp(tstart, "svg", 3) && !root_seen)
        {
            root_seen = true;
            if (!have_vb)
            {
                vb_w = (svg_w > 0.0f) ? svg_w : 32.0f;
                vb_h = (svg_h > 0.0f) ? svg_h : 32.0f;
            }
            float sx = (float)icon_w / vb_w;
            float sy = (float)icon_h / vb_h;
            float sc = (sx < sy) ? sx : sy;
            float ox = 0.5f * ((float)icon_w - vb_w * sc) - vb_x * sc;
            float oy = 0.5f * ((float)icon_h - vb_h * sc) - vb_y * sc;
            cur.xform = mat_mul((struct svg_mat){ sc, 0, 0, sc, ox, oy }, cur.xform);
        }

        /* Push container style */
        if (!self_closing &&
            ((tlen == 1 && !strncasecmp(tstart, "g", 1)) ||
             (tlen == 3 && !strncasecmp(tstart, "svg", 3)) ||
             (tlen == 1 && !strncasecmp(tstart, "a", 1))))
        {
            if (sp + 1 < SVG_MAX_STACK)
                stack[++sp] = cur;
            continue;
        }

        /* Render leaf shapes */
        if (tlen == 4 && !strncasecmp(tstart, "path", 4) && d_start && d_len > 0)
        {
            if (d_len + 1 > d_cap)
            {
                char *nb = realloc(d_buf, d_len + 1);
                if (!nb) continue;
                d_buf = nb;
                d_cap = d_len + 1;
            }
            memcpy(d_buf, d_start, d_len);
            d_buf[d_len] = '\0';
            parse_svg_path_d(d_buf, cur.xform, &pb);
            draw_shape(rgba, icon_w, icon_h, &cur, &pb, &stroke_pb);
        }
        else if (tlen == 4 && !strncasecmp(tstart, "rect", 4))
        {
            build_rect(&pb, cur.xform, sh_x, sh_y, sh_w, sh_h, sh_rx, sh_ry);
            draw_shape(rgba, icon_w, icon_h, &cur, &pb, &stroke_pb);
        }
        else if (tlen == 6 && !strncasecmp(tstart, "circle", 6))
        {
            build_ellipse(&pb, cur.xform, sh_cx, sh_cy, sh_r, sh_r);
            draw_shape(rgba, icon_w, icon_h, &cur, &pb, &stroke_pb);
        }
        else if (tlen == 7 && !strncasecmp(tstart, "ellipse", 7))
        {
            build_ellipse(&pb, cur.xform, sh_cx, sh_cy, sh_rx, sh_ry);
            draw_shape(rgba, icon_w, icon_h, &cur, &pb, &stroke_pb);
        }
        else if (tlen == 4 && !strncasecmp(tstart, "line", 4))
        {
            path_clear(&pb);
            float tx, ty;
            mat_point(cur.xform, sh_x1, sh_y1, &tx, &ty);
            path_begin_sub(&pb, tx, ty);
            mat_point(cur.xform, sh_x2, sh_y2, &tx, &ty);
            path_add_pt(&pb, tx, ty);
            struct svg_style ls = cur;
            ls.fill_kind = PAINT_NONE;
            draw_shape(rgba, icon_w, icon_h, &ls, &pb, &stroke_pb);
        }
        else if (((tlen == 7 && !strncasecmp(tstart, "polygon", 7)) ||
                  (tlen == 8 && !strncasecmp(tstart, "polyline", 8))) &&
                 d_start && d_len > 0)
        {
            if (d_len + 1 > d_cap)
            {
                char *nb = realloc(d_buf, d_len + 1);
                if (!nb) continue;
                d_buf = nb;
                d_cap = d_len + 1;
            }
            memcpy(d_buf, d_start, d_len);
            d_buf[d_len] = '\0';
            path_clear(&pb);
            const char *pp = d_buf;
            float px, py, fx = 0, fy = 0;
            bool first = true;
            while (parse_num(&pp, &px) && parse_num(&pp, &py))
            {
                float tx, ty;
                mat_point(cur.xform, px, py, &tx, &ty);
                if (first) { path_begin_sub(&pb, tx, ty); fx = tx; fy = ty; first = false; }
                else path_add_pt(&pb, tx, ty);
            }
            if (!first && tlen == 7)
            {
                path_add_pt(&pb, fx, fy);
                path_close_sub(&pb);
            }
            draw_shape(rgba, icon_w, icon_h, &cur, &pb, &stroke_pb);
        }
    }

    path_free(&pb);
    path_free(&stroke_pb);
    free(d_buf);

    /* Convert RGBA canvas to monochrome foreground ink coverage (0..255):
     * - White (#FFFFFF) frame backgrounds (as in Figma exports) -> ink 0
     * - Transparent (A=0) backgrounds -> ink 0
     * - Dark strokes/fills (#000000) -> ink 255
     * - White-on-transparent icons (max_ink == 0 && max_alpha > 0) -> use A */
    int npix = icon_w * icon_h;
    uint8_t max_ink = 0, max_a = 0;
    for (int i = 0; i < npix; i++)
    {
        uint8_t r = rgba[i * 4 + 0], g = rgba[i * 4 + 1];
        uint8_t b = rgba[i * 4 + 2], a = rgba[i * 4 + 3];
        uint32_t lum = (77u * r + 150u * g + 29u * b + 128u) >> 8;
        uint8_t ink = (uint8_t)(((255u - lum) * a + 127u) / 255u);
        if (ink > max_ink) max_ink = ink;
        if (a > max_a) max_a = a;
    }
    if (max_a == 0)
    {
        free(rgba);
        return false;
    }

    bool use_alpha_only = (max_ink == 0);
    for (int i = 0; i < npix; i += 2)
    {
        unsigned nib[2];
        for (int k = 0; k < 2; k++)
        {
            uint8_t *px = rgba + (i + k) * 4;
            uint32_t ink;
            if (use_alpha_only)
            {
                ink = px[3];
            }
            else
            {
                uint32_t lum = (77u * px[0] + 150u * px[1] + 29u * px[2] + 128u) >> 8;
                ink = ((255u - lum) * px[3] + 127u) / 255u;
            }
            /* Rockbox 4-bit alpha: 0 = fully opaque ink, 15 = fully transparent */
            unsigned q4 = (ink * 15u + 127u) / 255u;
            nib[k] = 15u - q4;
        }
        out_alpha4[i / 2] = (unsigned char)((nib[0] & 0x0f) | ((nib[1] & 0x0f) << 4));
    }

    free(rgba);
    return true;
}

/* ---- lazy icon cache (keyed by icon basename + target height) ---------- */

static void probe_svg_aspect(const char *xml, float *out_w, float *out_h)
{
    *out_w = 32.0f;
    *out_h = 32.0f;
    const char *p = xml;
    while ((p = strchr(p, '<')) != NULL)
    {
        p++;
        if (*p == '?' || *p == '!' || *p == '/')
            continue;
        if (!strncasecmp(p, "svg", 3) &&
            (isspace((unsigned char)p[3]) || p[3] == '>' || p[3] == '/'))
        {
            p += 3;
            char quote = 0;
            const char *tend = p;
            while (*tend && (quote || *tend != '>'))
            {
                if (quote) { if (*tend == quote) quote = 0; }
                else if (*tend == '"' || *tend == '\'') quote = *tend;
                tend++;
            }
            float svg_w = 0.0f, svg_h = 0.0f;
            while (p < tend)
            {
                while (p < tend && isspace((unsigned char)*p)) p++;
                if (p >= tend || *p == '/') break;
                const char *ks = p;
                while (p < tend && !isspace((unsigned char)*p) && *p != '=' && *p != '/') p++;
                size_t kl = (size_t)(p - ks);
                while (p < tend && isspace((unsigned char)*p)) p++;
                if (p >= tend || *p != '=') continue;
                p++;
                while (p < tend && isspace((unsigned char)*p)) p++;
                if (p >= tend) break;
                const char *vs;
                size_t vl = 0;
                if (*p == '"' || *p == '\'')
                {
                    char q = *p++;
                    vs = p;
                    while (p < tend && *p != q) p++;
                    vl = (size_t)(p - vs);
                    if (p < tend && *p == q) p++;
                }
                else
                {
                    vs = p;
                    while (p < tend && !isspace((unsigned char)*p)) p++;
                    vl = (size_t)(p - vs);
                }
                char vbuf[128];
                size_t cl = vl < sizeof(vbuf) - 1 ? vl : sizeof(vbuf) - 1;
                memcpy(vbuf, vs, cl);
                vbuf[cl] = '\0';
                const char *vp = vbuf;
                if (kl == 7 && !strncasecmp(ks, "viewBox", 7))
                {
                    float vx, vy, vw, vh;
                    if (parse_num(&vp, &vx) && parse_num(&vp, &vy) &&
                        parse_num(&vp, &vw) && parse_num(&vp, &vh) &&
                        vw > 0.0f && vh > 0.0f)
                    {
                        *out_w = vw;
                        *out_h = vh;
                        return;
                    }
                }
                else if (kl == 5 && !strncasecmp(ks, "width", 5))
                    parse_num(&vp, &svg_w);
                else if (kl == 6 && !strncasecmp(ks, "height", 6))
                    parse_num(&vp, &svg_h);
            }
            if (svg_w > 0.0f && svg_h > 0.0f)
            {
                *out_w = svg_w;
                *out_h = svg_h;
            }
            return;
        }
    }
}

struct svg_cache_entry
{
    char key[SVG_KEY_MAX];
    int  target_h;
    bool valid;
    struct trimpod_svg_icon icon;
};

static struct svg_cache_entry svg_cache[SVG_CACHE_SLOTS];
static int svg_cache_count;

const struct trimpod_svg_icon *trimpod_svg_load(const char *name,
                                                int target_h)
{
    if (!name || !*name || target_h <= 0)
        return NULL;

    for (int i = 0; i < svg_cache_count; i++)
        if (svg_cache[i].target_h == target_h &&
            strcmp(svg_cache[i].key, name) == 0)
            return svg_cache[i].valid ? &svg_cache[i].icon : NULL;

    if (svg_cache_count >= SVG_CACHE_SLOTS)
        return NULL;

    struct svg_cache_entry *e = &svg_cache[svg_cache_count++];
    snprintf(e->key, sizeof(e->key), "%s", name);
    snprintf(e->icon.name, sizeof(e->icon.name), "%s", name);
    e->target_h = target_h;
    e->valid = false;

    char path[256];
    snprintf(path, sizeof(path), "%s/icons/%s.svg", ROCKBOX_DIR, name);
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return NULL;

    char *buf = malloc(SVG_MAX_FILE_BYTES);
    if (!buf)
    {
        close(fd);
        return NULL;
    }
    ssize_t n = read(fd, buf, SVG_MAX_FILE_BYTES - 1);
    close(fd);
    if (n <= 0)
    {
        free(buf);
        return NULL;
    }
    buf[n] = '\0';

    float aw = 32.0f, ah = 32.0f;
    probe_svg_aspect(buf, &aw, &ah);
    int icon_h = target_h;
    int icon_w = (int)floorf((float)target_h * (aw / ah) + 0.5f);
    if (icon_w < 2) icon_w = 2;
    icon_w = (icon_w + 1) & ~1;  /* even width for packed 4-bit alpha stride */

    unsigned char *alpha = malloc((size_t)(icon_w * icon_h) / 2);
    if (!alpha)
    {
        free(buf);
        return NULL;
    }

    e->icon.width  = icon_w;
    e->icon.height = icon_h;
    e->icon.alpha  = alpha;
    e->valid = rasterize_svg_text(buf, icon_w, icon_h, alpha);
    free(buf);
    if (!e->valid)
    {
        free(alpha);
        e->icon.alpha = NULL;
        return NULL;
    }
    return &e->icon;
}

static const struct trimpod_svg_icon *svg_tag_descriptor(const char *tag)
{
    static struct trimpod_svg_icon tag_cache[SVG_CACHE_SLOTS];
    static int tag_count;

    const struct trimpod_svg_icon *loaded =
        trimpod_svg_load(tag, TRIMPOD_SVG_ICON_SIZE);
    if (loaded)
        return loaded;

    for (int i = 0; i < tag_count; i++)
        if (strcmp(tag_cache[i].name, tag) == 0)
            return &tag_cache[i];

    if (tag_count >= SVG_CACHE_SLOTS)
        return NULL;

    struct trimpod_svg_icon *slot = &tag_cache[tag_count++];
    memset(slot, 0, sizeof(*slot));
    snprintf(slot->name, sizeof(slot->name), "%s", tag);
    return slot;
}

const struct trimpod_svg_icon *trimpod_svg_tag(const char *tag)
{
    if (!tag || !*tag)
        return NULL;
    return svg_tag_descriptor(tag);
}

const struct trimpod_svg_icon *trimpod_svg_for_path(const char *path,
                                                    bool is_dir)
{
    if (is_dir)
        return svg_tag_descriptor("folder");
    if (!path || !*path)
        return NULL;

    const char *slash = strrchr(path, '/');
    const char *base  = slash ? slash + 1 : path;
    const char *dot   = strrchr(base, '.');
    if (!dot || dot == base || !dot[1])
        return NULL;

    char ext[SVG_KEY_MAX];
    size_t i = 0;
    for (const char *s = dot + 1; *s && i < sizeof(ext) - 1; s++, i++)
        ext[i] = (char)tolower((unsigned char)*s);
    ext[i] = '\0';

    return svg_tag_descriptor(ext);
}

const struct trimpod_svg_icon *trimpod_svg_toggle(bool enabled)
{
    return trimpod_svg_load(enabled ? "toggle_enabled" : "toggle",
                            TRIMPOD_SVG_TOGGLE_H);
}

#ifndef TRIMPOD_SVG_HOST_TEST
void trimpod_svg_draw(struct screen *display,
                      const struct trimpod_svg_icon *icon,
                      int x, int y, bool invert)
{
    if (!display || !icon || !icon->alpha)
        return;
    display->set_drawmode(invert ? (DRMODE_SOLID | DRMODE_INVERSEVID) : DRMODE_FG);
    lcd_alpha_bitmap_part(icon->alpha, 0, 0, icon->width,
                          x, y, icon->width, icon->height);
    display->set_drawmode(DRMODE_SOLID);
}
#else
int main(int argc, char **argv)
{
    if (argc < 3) return 1;
    FILE *f = fopen(argv[1], "rb");
    if (!f) return 2;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *xml = malloc((size_t)sz + 1);
    fread(xml, 1, (size_t)sz, f);
    xml[sz] = '\0';
    fclose(f);

    unsigned char alpha[(TRIMPOD_SVG_ICON_SIZE * TRIMPOD_SVG_ICON_SIZE) / 2];
    if (!rasterize_svg_text(xml, TRIMPOD_SVG_ICON_SIZE, TRIMPOD_SVG_ICON_SIZE, alpha))
        return 3;

    FILE *out = fopen(argv[2], "wb");
    fprintf(out, "P5\n%d %d\n255\n", TRIMPOD_SVG_ICON_SIZE, TRIMPOD_SVG_ICON_SIZE);
    for (int i = 0; i < (TRIMPOD_SVG_ICON_SIZE * TRIMPOD_SVG_ICON_SIZE) / 2; i++)
    {
        unsigned char n0 = (15 - (alpha[i] & 0x0f)) * 17;
        unsigned char n1 = (15 - ((alpha[i] >> 4) & 0x0f)) * 17;
        fputc(n0, out);
        fputc(n1, out);
    }
    fclose(out);
    free(xml);
    return 0;
}
#endif
