import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { FilesDeleteRequest } from "../models/operations/filesdelete.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Delete File
 *
 * @remarks
 * Delete a file.
 *
 * **Scopes**: `files:write`
 */
export declare function filesDelete(client: PolarCore, request: FilesDeleteRequest, options?: RequestOptions): APIPromise<Result<void, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=filesDelete.d.ts.map