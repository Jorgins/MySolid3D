#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepFilletAPI_MakeFillet2d.hxx>
#include <bindings_common.hxx>

inline std::unique_ptr<TopoDS_Edge> BRepFilletAPI_MakeFillet2d_add_fillet(BRepFilletAPI_MakeFillet2d &make_fillet,
                                                                          const TopoDS_Vertex &vertex,
                                                                          Standard_Real radius) {
  return std::unique_ptr<TopoDS_Edge>(new TopoDS_Edge(make_fillet.AddFillet(vertex, radius)));
}

// Chamfers
inline std::unique_ptr<TopoDS_Edge>
BRepFilletAPI_MakeFillet2d_add_chamfer(BRepFilletAPI_MakeFillet2d &make_fillet, const TopoDS_Edge &edge1,
                                       const TopoDS_Edge &edge2, const Standard_Real dist1, const Standard_Real dist2) {
  return std::unique_ptr<TopoDS_Edge>(new TopoDS_Edge(make_fillet.AddChamfer(edge1, edge2, dist1, dist2)));
}

inline std::unique_ptr<TopoDS_Edge>
BRepFilletAPI_MakeFillet2d_add_chamfer_angle(BRepFilletAPI_MakeFillet2d &make_fillet, const TopoDS_Edge &edge,
                                             const TopoDS_Vertex &vertex, const Standard_Real dist,
                                             const Standard_Real angle) {
  return std::unique_ptr<TopoDS_Edge>(new TopoDS_Edge(make_fillet.AddChamfer(edge, vertex, dist, angle)));
}

// Построение с проверкой: при ошибке OCCT бросает Standard_Failure (не std::exception),
// что через границу FFI завершило бы процесс. Возвращаем nullptr вместо исключения.
template <typename Maker> inline std::unique_ptr<TopoDS_Shape> build_shape_checked(Maker &maker) {
  try {
    maker.Build(Message_ProgressRange());
    if (!maker.IsDone()) {
      return nullptr;
    }
    return std::unique_ptr<TopoDS_Shape>(new TopoDS_Shape(maker.Shape()));
  } catch (...) {
    return nullptr;
  }
}

inline std::unique_ptr<TopoDS_Shape> BRepFilletAPI_MakeFillet_build_checked(BRepFilletAPI_MakeFillet &maker) {
  return build_shape_checked(maker);
}

inline std::unique_ptr<TopoDS_Shape> BRepFilletAPI_MakeChamfer_build_checked(BRepFilletAPI_MakeChamfer &maker) {
  return build_shape_checked(maker);
}
