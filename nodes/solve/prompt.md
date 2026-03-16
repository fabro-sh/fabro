Goal: Autoreloader with StatReloader doesn't track changes in manage.py.
Description
	 
		(last modified by Mariusz Felisiak)
	 
This is a bit convoluted, but here we go.
Environment (OSX 10.11):
$ python -V
Python 3.6.2
$ pip -V
pip 19.1.1
$ pip install Django==2.2.1
Steps to reproduce:
Run a server python manage.py runserver
Edit the manage.py file, e.g. add print(): 
def main():
	print('sth')
	os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'ticket_30479.settings')
	...
Under 2.1.8 (and prior), this will trigger the auto-reloading mechanism. Under 2.2.1, it won't. As far as I can tell from the django.utils.autoreload log lines, it never sees the manage.py itself.



## Additional Context

Thanks for the report. I simplified scenario. Regression in c8720e7696ca41f3262d5369365cc1bd72a216ca. Reproduced at 8d010f39869f107820421631111417298d1c5bb9.
Argh. I guess this is because manage.py isn't showing up in the sys.modules. I'm not sure I remember any specific manage.py handling in the old implementation, so I'm not sure how it used to work, but I should be able to fix this pretty easily.
Done a touch of debugging: iter_modules_and_files is where it gets lost. Specifically, it ends up in there twice: (<module '__future__' from '/../lib/python3.6/__future__.py'>, <module '__main__' from 'manage.py'>, <module '__main__' from 'manage.py'>, ...,) But getattr(module, "__spec__", None) is None is True so it continues onwards. I thought I managed to get one of them to have a __spec__ attr but no has_location, but I can't seem to get that again (stepping around with pdb) Digging into wtf __spec__ is None: ​Here's the py3 docs on it, which helpfully mentions that ​The one exception is __main__, where __spec__ is set to None in some cases
Tom, will you have time to work on this in the next few days?
I'm sorry for assigning it to myself Mariusz, I intended to work on it on Tuesday but work overtook me and now I am travelling for a wedding this weekend. So I doubt it I'm afraid. It seems Keryn's debugging is a great help, it should be somewhat simple to add special case handling for __main__, while __spec__ is None we can still get the filename and watch on that.
np, Tom, thanks for info. Keryn, it looks that you've already made most of the work. Would you like to prepare a patch?

## Completed stages
- **setup**: fail
  - Script: `git clone https://github.com/django/django.git . && git checkout df46b329e0900e9e4dc1d60816c1dce6dfc1094e && python -m pip install -e .`
  - Stdout:
    ```
    fatal: destination path '.' already exists and is not an empty directory.
    ```
  - Stderr: (empty)

## Context
- failure_class: deterministic
- failure_signature: setup|deterministic|script failed with exit code: <n> ## stdout fatal: destination path '.' already exists and is not an empty directory.


Fix this GitHub issue in the repository. Make the minimal code change needed.