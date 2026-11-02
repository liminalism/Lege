// SPDX-License-Identifier: MIT
// Benchmark harness only. The external Apache jbig2enc sources are not bundled.
#include "jbig2arith.h"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <stdexcept>
#include <string>
#include <vector>

struct Page { unsigned width,height; std::vector<uint32_t> words; };
static uint32_t number(std::istream &input) {
    unsigned char b[4]; input.read(reinterpret_cast<char*>(b),4);
    if(!input) throw std::runtime_error("truncated packed corpus");
    return uint32_t(b[0])|(uint32_t(b[1])<<8)|(uint32_t(b[2])<<16)|(uint32_t(b[3])<<24);
}
static std::vector<uint8_t> encode(const Page &page) {
    jbig2enc_ctx ctx; jbig2enc_init(&ctx);
    jbig2enc_bitimage(&ctx,reinterpret_cast<const uint8_t*>(page.words.data()),page.width,page.height,false);
    jbig2enc_final(&ctx); std::vector<uint8_t> output(jbig2enc_datasize(&ctx));
    jbig2enc_tobuffer(&ctx,output.data()); jbig2enc_dealloc(&ctx); return output;
}
int main(int argc,char **argv) {
    if(argc!=3 && argc!=4) throw std::runtime_error("input rounds [output-directory]");
    std::ifstream input(argv[1],std::ios::binary); char magic[8]; input.read(magic,8);
    if(std::string(magic,8)!="GREGION1") throw std::runtime_error("bad corpus magic");
    unsigned count=number(input);std::vector<Page> pages;
    for(unsigned i=0;i<count;++i) {
        Page page;page.width=number(input);page.height=number(input);unsigned words=number(input);
        if(words!=((page.width+31)/32)*page.height)throw std::runtime_error("bad packed size");
        for(unsigned j=0;j<words;++j)page.words.push_back(number(input));pages.push_back(std::move(page));
    }
    unsigned rounds=std::stoul(argv[2]);if(!rounds)throw std::runtime_error("zero rounds");
    for(const auto &page:pages){auto output=encode(page);asm volatile(""::"g"(output.data()):"memory");}
    auto start=std::chrono::steady_clock::now();uint64_t bytes=0;
    for(unsigned r=0;r<rounds;++r)for(const auto &page:pages){auto output=encode(page);bytes+=output.size();asm volatile(""::"g"(output.data()):"memory");}
    double seconds=std::chrono::duration<double>(std::chrono::steady_clock::now()-start).count();
    printf("GENERIC_RESULT\t%.9f\t%llu\t%u\t%zu\n",seconds,(unsigned long long)bytes,rounds,pages.size());
    if(argc==4)for(size_t i=0;i<pages.size();++i){auto output=encode(pages[i]);char name[40];snprintf(name,sizeof(name),"/%04zu.bin",i);std::ofstream file(std::string(argv[3])+name,std::ios::binary);file.write(reinterpret_cast<const char*>(output.data()),output.size());if(!file)throw std::runtime_error("output write failed");}
}
