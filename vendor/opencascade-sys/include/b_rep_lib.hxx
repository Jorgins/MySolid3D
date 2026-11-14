#include <BRepLib.hxx>
#include <BRepLib_ToolTriangulatedShape.hxx>
#include <TopoDS_Shape.hxx>
#include <bindings_common.hxx>
#include <Poly_Triangulation.hxx>
#include <TopoDS_Face.hxx>

// MSVC: Handle_Poly_Triangulation — отдельный класс, поэтому адрес перегруженного
// статического метода взять нельзя; вызываем через обёртку.
inline void BRepLib_ToolTriangulatedShape_ComputeNormals(const TopoDS_Face &face,
                                                         const Handle_Poly_Triangulation &triangulation) {
  BRepLib_ToolTriangulatedShape::ComputeNormals(face, triangulation);
}
