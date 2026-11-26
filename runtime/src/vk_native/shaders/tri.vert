#version 450
// Built-in smoke-test vertex shader for the native backend. Compiled with:
//   glslc tri.vert -o tri.vert.spv
// (a real D3D11 pipeline will feed SPIR-V translated from the game's DXBC).
layout(location = 0) in vec2 in_pos;
layout(location = 1) in vec3 in_col;
layout(location = 0) out vec3 v_col;
void main() {
    v_col = in_col;
    gl_Position = vec4(in_pos, 0.0, 1.0);
}
