// Rounded IEEE-754 arithmetic prevents backend contraction from moving YUV quantization boundaries.
fn ieee_mul_bits(a:u32,b:u32)->u32 {
 let sign=(a^b)&0x80000000u; let aa=a&0x7fffffffu; let bb=b&0x7fffffffu;
 if(aa>0x7f800000u || bb>0x7f800000u || (aa==0u && bb==0x7f800000u) || (bb==0u && aa==0x7f800000u)){return 0x7fc00000u;}
 if(aa==0x7f800000u || bb==0x7f800000u){return sign|0x7f800000u;}
 if(aa==0u || bb==0u){return sign;}
 var ma=aa&0x7fffffu; var mb=bb&0x7fffffu;
 var ea=i32(aa>>23u)-127; var eb=i32(bb>>23u)-127;
 if((aa>>23u)==0u){ea=-126;loop{if(ma>=0x800000u){break;}ma=ma<<1u;ea-=1;}}else{ma|=0x800000u;}
 if((bb>>23u)==0u){eb=-126;loop{if(mb>=0x800000u){break;}mb=mb<<1u;eb-=1;}}else{mb|=0x800000u;}
 let p0=(ma&0xffffu)*(mb&0xffffu); let p1=(ma>>16u)*(mb&0xffffu);
 let p2=(ma&0xffffu)*(mb>>16u); let p3=(ma>>16u)*(mb>>16u);
 var lo=p0; var hi=p3+(p1>>16u)+(p2>>16u);
 var prev=lo;lo+=p1<<16u;if(lo<prev){hi+=1u;}
 prev=lo;lo+=p2<<16u;if(lo<prev){hi+=1u;}
 var exponent=ea+eb;var base_shift=23u;
 if((hi&0x8000u)!=0u){exponent+=1;base_shift=24u;}
 if(exponent>127){return sign|0x7f800000u;}if(exponent < -150){return sign;}
 let product_shift=base_shift+u32(max(-126-exponent,0));
 var q=0u;
 if(product_shift<32u){q=(hi<<(32u-product_shift))|(lo>>product_shift);}else{q=hi>>(product_shift-32u);}
 var guard=0u;var sticky=false;
 if(product_shift<=32u){guard=(lo>>(product_shift-1u))&1u;sticky=(lo&((1u<<(product_shift-1u))-1u))!=0u;}
 else{guard=(hi>>(product_shift-33u))&1u;sticky=lo!=0u || (hi&((1u<<(product_shift-33u))-1u))!=0u;}
 if(guard!=0u && (sticky || (q&1u)!=0u)){q+=1u;}
 if(exponent < -126){return sign|q;}
 if(q==0x1000000u){q=q>>1u;exponent+=1;}
 if(exponent>127){return sign|0x7f800000u;}
 return sign|(u32(exponent+127)<<23u)|(q&0x7fffffu);
}
