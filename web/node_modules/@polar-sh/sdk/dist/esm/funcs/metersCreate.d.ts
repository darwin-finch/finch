import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Meter } from "../models/components/meter.js";
import { MeterCreate } from "../models/components/metercreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Meter
 *
 * @remarks
 * Create a meter.
 *
 * **Scopes**: `meters:write`
 */
export declare function metersCreate(client: PolarCore, request: MeterCreate, options?: RequestOptions): APIPromise<Result<Meter, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=metersCreate.d.ts.map