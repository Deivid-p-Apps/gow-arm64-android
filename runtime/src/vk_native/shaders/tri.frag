#version 450
// Built-in smoke-test fragment shader. Compiled with:
//   glslc tri.frag -o tri.frag.spv
layout(location = 0) in vec3 v_col;
layout(location = 0) out vec4 out_col;
void main() { out_col = vec4(v_col, 1.0); }
