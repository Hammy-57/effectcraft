// Rounded IEEE-754 arithmetic prevents backend contraction from moving YUV quantization boundaries.
fn add_shift_jam(value:u32, distance:u32)->u32 {
 if(distance==0u){return value;}
 if(distance>=32u){return select(0u,1u,value!=0u);}
 let discarded=(value&((1u<<distance)-1u))!=0u;
 return (value>>distance)|select(0u,1u,discarded);
}
fn ieee_add_bits(a:u32,b:u32)->u32 {
 let aa=a&0x7fffffffu;let bb=b&0x7fffffffu;
 if(aa>0x7f800000u || bb>0x7f800000u){return 0x7fc00000u;}
 if(aa==0x7f800000u || bb==0x7f800000u){
  if(aa==0x7f800000u && bb==0x7f800000u && ((a^b)&0x80000000u)!=0u){return 0x7fc00000u;}
  return select(b,a,aa==0x7f800000u);
 }
 if(aa==0u && bb==0u){return (a&b)&0x80000000u;}
 if(aa==0u){return b;}if(bb==0u){return a;}
 var large=a;var small=b;if(aa<bb){large=b;small=a;}
 let lm=large&0x7fffffffu;let sm=small&0x7fffffffu;
 var exponent=max(lm>>23u,1u);let small_exponent=max(sm>>23u,1u);
 let lmant=(lm&0x7fffffu)|select(0u,0x800000u,(lm>>23u)!=0u);
 let smant=(sm&0x7fffffu)|select(0u,0x800000u,(sm>>23u)!=0u);
 let aligned=add_shift_jam(smant<<3u,exponent-small_exponent);
 var sig=0u;
 if(((large^small)&0x80000000u)==0u){
  sig=(lmant<<3u)+aligned;
  if((sig&0x8000000u)!=0u){sig=add_shift_jam(sig,1u);exponent+=1u;}
 }else{
  sig=(lmant<<3u)-aligned;if(sig==0u){return 0u;}
  let shift=min(exponent-1u,countLeadingZeros(sig)-5u);sig=sig<<shift;exponent-=shift;
 }
 var rounded=sig>>3u;let tail=sig&7u;
 if(tail>4u || (tail==4u && (rounded&1u)!=0u)){rounded+=1u;}
 if(rounded>=0x1000000u){rounded=rounded>>1u;exponent+=1u;}
 let sign=large&0x80000000u;
 if(exponent>=255u){return sign|0x7f800000u;}
 if(rounded<0x800000u){return sign|rounded;}
 return sign|(exponent<<23u)|(rounded&0x7fffffu);
}
fn ieee_sub_bits(a:u32,b:u32)->u32 {return ieee_add_bits(a,b^0x80000000u);}
