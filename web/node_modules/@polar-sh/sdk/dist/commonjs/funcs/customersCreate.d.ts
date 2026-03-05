import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Customer } from "../models/components/customer.js";
import { CustomerCreate } from "../models/components/customercreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Customer
 *
 * @remarks
 * Create a customer.
 *
 * **Scopes**: `customers:write`
 */
export declare function customersCreate(client: PolarCore, request: CustomerCreate, options?: RequestOptions): APIPromise<Result<Customer, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customersCreate.d.ts.map