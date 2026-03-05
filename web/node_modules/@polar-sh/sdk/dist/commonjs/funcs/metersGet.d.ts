import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Meter } from "../models/components/meter.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { MetersGetRequest } from "../models/operations/metersget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Meter
 *
 * @remarks
 * Get a meter by ID.
 *
 * **Scopes**: `meters:read` `meters:write`
 */
export declare function metersGet(client: PolarCore, request: MetersGetRequest, options?: RequestOptions): APIPromise<Result<Meter, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=metersGet.d.ts.map