(function(){
  var r=[], p=app.project;
  r.push("project: "+(p.file ? p.file.fsName : "(unsaved, no file)"));
  r.push("unsaved changes: "+(p.dirty ? "YES — ask before closing or saving" : "no"));
  r.push("items: "+p.numItems);
  var rq=p.renderQueue; r.push("render queue items: "+rq.numItems);
  for(var i=1;i<=rq.numItems;i++){ var it=rq.item(i);
    r.push("  "+i+". "+(it.comp?it.comp.name:"?")+" | status "+it.status+" | render "+it.render); }
  var want=["Inter"], all=null;
  try{ all=app.fonts.allFonts; }catch(e){ r.push("fonts: API unavailable ("+e.toString()+")"); }
  for(var j=0;all&&j<want.length;j++){
    var hits=[];
    for(var g=0;g<all.length;g++) for(var k=0;k<all[g].length;k++)
      if(all[g][k].familyName===want[j]) hits.push(all[g][k].postScriptName);
    r.push("font "+want[j]+": "+(hits.length?hits.join(", "):"NOT INSTALLED"));
  }
  var f=new File("Inter/preflight.txt"); f.encoding="UTF-8"; f.open("w"); f.write(r.join("\n")); f.close();
})();
