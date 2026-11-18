/**
 * WebGL 渲染器:把 WASM 输出的 R8 图集上传为 LUMINANCE 纹理,
 * 逐字形用带 alpha 的四边形绘制到画布。
 */

const VERT = `
attribute vec2 a_pos;
attribute vec2 a_uv;
uniform vec2 u_viewport;
varying vec2 v_uv;
void main() {
  vec2 clip = (a_pos / u_viewport) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_uv = a_uv;
}`;

const FRAG = `
precision mediump float;
uniform sampler2D u_tex;
uniform vec4 u_color;
varying vec2 v_uv;
void main() {
  float a = texture2D(u_tex, v_uv).r;
  gl_FragColor = vec4(u_color.rgb, u_color.a * a);
}`;

function compile(gl: WebGLRenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type)!;
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    throw new Error('shader 编译失败: ' + gl.getShaderInfoLog(sh));
  }
  return sh;
}

export interface PlacedGlyph {
  /** 图集中的像素矩形 */
  sx: number; sy: number; sw: number; sh: number;
  /** 画布目标矩形(像素) */
  dx: number; dy: number; dw: number; dh: number;
}

export class AtlasRenderer {
  private gl: WebGLRenderingContext;
  private program: WebGLProgram;
  private texture: WebGLTexture;
  private atlasW = 0;
  private atlasH = 0;
  private loc: {
    pos: number; uv: number; viewport: WebGLUniformLocation | null;
    tex: WebGLUniformLocation | null; color: WebGLUniformLocation | null;
  };

  constructor(canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl', { premultipliedAlpha: false });
    if (!gl) throw new Error('当前浏览器不支持 WebGL');
    this.gl = gl;
    const vs = compile(gl, gl.VERTEX_SHADER, VERT);
    const fs = compile(gl, gl.FRAGMENT_SHADER, FRAG);
    const program = gl.createProgram()!;
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error('program 链接失败: ' + gl.getProgramInfoLog(program));
    }
    this.program = program;
    this.loc = {
      pos: gl.getAttribLocation(program, 'a_pos'),
      uv: gl.getAttribLocation(program, 'a_uv'),
      viewport: gl.getUniformLocation(program, 'u_viewport'),
      tex: gl.getUniformLocation(program, 'u_tex'),
      color: gl.getUniformLocation(program, 'u_color'),
    };
    this.texture = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
  }

  /** 用最新 R8 像素刷新纹理(尺寸变化时重新分配)。 */
  uploadAtlas(data: Uint8Array, width: number, height: number): void {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    if (width !== this.atlasW || height !== this.atlasH) {
      // LUMINANCE 把单通道复制为 rgb,alpha 取 1(在 shader 里用 .r 做覆盖率)
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.LUMINANCE, width, height, 0, gl.LUMINANCE, gl.UNSIGNED_BYTE, data);
      this.atlasW = width;
      this.atlasH = height;
    } else {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.LUMINANCE, gl.UNSIGNED_BYTE, data);
    }
  }

  /** 清空画布(透明)。 */
  clearRect(w: number, h: number): void {
    const gl = this.gl;
    gl.viewport(0, 0, w, h);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
  }

  /** 清屏并绘制一批字形四边形。 */
  draw(glyphs: PlacedGlyph[], color: [number, number, number, number], canvasW: number, canvasH: number): void {
    const gl = this.gl;
    gl.viewport(0, 0, canvasW, canvasH);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.program);
    gl.uniform2f(this.loc.viewport, canvasW, canvasH);
    gl.uniform4f(this.loc.color, color[0], color[1], color[2], color[3]);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.uniform1i(this.loc.tex, 0);

    // 每个四边形两个三角形,每顶点 pos(2)+uv(2)
    const verts: number[] = [];
    for (const g of glyphs) {
      const u0 = g.sx / this.atlasW;
      const v0 = g.sy / this.atlasH;
      const u1 = (g.sx + g.sw) / this.atlasW;
      const v1 = (g.sy + g.sh) / this.atlasH;
      const { dx, dy, dw, dh } = g;
      const p00 = [dx, dy], p10 = [dx + dw, dy], p01 = [dx, dy + dh], p11 = [dx + dw, dy + dh];
      const tri = [
        [...p00, u0, v0], [...p10, u1, v0], [...p01, u0, v1],
        [...p10, u1, v0], [...p11, u1, v1], [...p01, u0, v1],
      ];
      for (const v of tri) verts.push(...v);
    }
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.STREAM_DRAW);
    const stride = 4 * 4;
    gl.enableVertexAttribArray(this.loc.pos);
    gl.vertexAttribPointer(this.loc.pos, 2, gl.FLOAT, false, stride, 0);
    gl.enableVertexAttribArray(this.loc.uv);
    gl.vertexAttribPointer(this.loc.uv, 2, gl.FLOAT, false, stride, 2 * 4);
    gl.drawArrays(gl.TRIANGLES, 0, glyphs.length * 6);
    gl.deleteBuffer(buf);
  }
}
