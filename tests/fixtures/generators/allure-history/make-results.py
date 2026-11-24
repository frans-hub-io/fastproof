import json,sys,uuid,time,os,hashlib
build=int(sys.argv[1]); out=sys.argv[2]
os.makedirs(out,exist_ok=True)
tests=[("com.example.LoginTest.logsIn","passed"),("com.example.LoginTest.rejectsBadPassword","passed"),("com.example.CartTest.addsItem",["passed","failed","passed"][build%3]),("com.example.CartTest.removesItem",["passed","passed","broken"][build%3]),("com.example.PayTest.charges","skipped" if build==2 else "passed")]
base=1700000000000+build*86400000
for i,(fn,st) in enumerate(tests):
    tcid=hashlib.md5(fn.encode()).hexdigest()
    r={"uuid":str(uuid.uuid4()),"historyId":hashlib.md5((tcid+"x").encode()).hexdigest(),"testCaseId":tcid,"fullName":fn,"name":fn.split(".")[-1],"status":st,"start":base+i*1000,"stop":base+i*1000+500+build*10,
       "labels":[{"name":"suite","value":fn.split(".")[-2]}],"statusDetails":{"message":"boom"} if st in("failed","broken") else {}}
    json.dump(r,open(f"{out}/{r['uuid']}-result.json","w"))
