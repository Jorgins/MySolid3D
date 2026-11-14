#include <BRepOffsetAPI_MakeOffset.hxx>
#include <BRepOffsetAPI_MakePipe.hxx>
#include <BRepOffsetAPI_MakePipeShell.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <Law_Function.hxx>
#include <TopTools_ListOfShape.hxx>
#include <TopoDS_Shape.hxx>
#include <bindings_common.hxx>

// MSVC: Handle_Law_Function — отдельный класс, адрес перегруженного метода SetLaw
// однозначно не выводится; вызываем через обёртку.
inline void BRepOffsetAPI_MakePipeShell_SetLaw(BRepOffsetAPI_MakePipeShell &shell, const TopoDS_Shape &profile,
                                               const Handle_Law_Function &law, bool with_contact,
                                               bool with_correction) {
  shell.SetLaw(profile, law, with_contact, with_correction);
}
